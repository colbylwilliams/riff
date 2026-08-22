import AVFoundation
import Foundation
import RiffCore

/// Microphone capture and playback for a Riff session.
///
/// The hard part of a voice agent on a phone is not moving bytes, it is keeping the agent from
/// hearing itself: without hardware echo cancellation the model's own voice comes back through the
/// microphone, the endpointer treats it as the speaker talking, and the agent interrupts itself
/// mid-sentence. That is why voice processing and the voice-chat audio session mode below are not
/// optional extras.
public final class RiffAudioEngine: @unchecked Sendable {
    public struct Configuration: Sendable {
        /// The rate the realtime API works in. Hardware rarely matches it, so capture is resampled.
        public var sampleRate: Double
        /// Roughly 20ms per chunk. Steady small chunks keep the endpointer responsive.
        public var chunkFrames: Int
        /// Hardware echo cancellation, noise suppression, and automatic gain control.
        public var voiceProcessing: Bool
        /// Route to the speaker rather than the earpiece, which is what people expect hands free.
        public var preferSpeaker: Bool

        public init(
            sampleRate: Double = 24000,
            chunkFrames: Int = 480,
            voiceProcessing: Bool = true,
            preferSpeaker: Bool = true
        ) {
            self.sampleRate = sampleRate
            self.chunkFrames = chunkFrames
            self.voiceProcessing = voiceProcessing
            self.preferSpeaker = preferSpeaker
        }
    }

    public enum AudioError: Error, CustomStringConvertible {
        case formatUnavailable
        case converterUnavailable
        case sessionConfiguration(String)

        public var description: String {
            switch self {
            case .formatUnavailable: "could not build the 24 kHz mono capture format"
            case .converterUnavailable: "could not build an audio converter for the input hardware"
            case .sessionConfiguration(let reason): "could not configure the audio session: \(reason)"
            }
        }
    }

    /// Captured audio as 16-bit little-endian PCM at the configured rate, ready to send.
    /// Called on the audio thread, so hop to your own actor before touching shared state.
    public var onCapture: (@Sendable (Data) -> Void)?
    /// Fires when the system takes the audio route away, so the session can be paused honestly.
    public var onInterruption: (@Sendable (Bool) -> Void)?

    private let engine = AVAudioEngine()
    private let player = AVAudioPlayerNode()
    private let configuration: Configuration
    private let state = AudioState()

    public init(configuration: Configuration = Configuration()) {
        self.configuration = configuration
    }

    public var isRunning: Bool { state.isRunning }

    public func start() throws {
        guard !state.isRunning else { return }

        try configureAudioSession()
        observeInterruptions()

        let input = engine.inputNode
        if configuration.voiceProcessing {
            // Without this the model's own output is picked up by the microphone and the endpointer
            // treats it as the speaker, so the agent talks over itself.
            try? input.setVoiceProcessingEnabled(true)
        }

        let hardwareFormat = input.outputFormat(forBus: 0)
        guard
            let capture = AVAudioFormat(
                commonFormat: .pcmFormatInt16,
                sampleRate: configuration.sampleRate,
                channels: 1,
                interleaved: true
            ),
            let playback = AVAudioFormat(
                commonFormat: .pcmFormatFloat32,
                sampleRate: configuration.sampleRate,
                channels: 1,
                interleaved: false
            )
        else { throw AudioError.formatUnavailable }

        guard let converter = AVAudioConverter(from: hardwareFormat, to: capture) else {
            throw AudioError.converterUnavailable
        }

        state.configure(capture: capture, playback: playback, converter: converter)

        engine.attach(player)
        engine.connect(player, to: engine.mainMixerNode, format: playback)

        let chunkBytes = configuration.chunkFrames * MemoryLayout<Int16>.size
        let state = self.state
        input.installTap(onBus: 0, bufferSize: 2048, format: hardwareFormat) { [weak self] buffer, _ in
            // Conversion happens on the audio thread and only the resulting bytes leave it, which
            // keeps capture latency off the main thread entirely.
            let chunks = state.convert(Unsafe(buffer), chunkBytes: chunkBytes)
            guard let handler = self?.onCapture else { return }
            for chunk in chunks { handler(chunk) }
        }

        engine.prepare()
        try engine.start()
        player.play()
        state.isRunning = true
    }

    public func stop() {
        guard state.isRunning else { return }
        engine.inputNode.removeTap(onBus: 0)
        player.stop()
        engine.stop()
        state.reset()
        removeObservers()

        #if os(iOS) || os(visionOS)
        try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
        #endif
    }

    /// Schedules 16-bit little-endian PCM from the agent for playback.
    public func play(_ pcm: Data) {
        guard state.isRunning, let playbackFormat = state.playbackFormat, !pcm.isEmpty else { return }

        let frames = pcm.count / MemoryLayout<Int16>.size
        guard frames > 0,
              let buffer = AVAudioPCMBuffer(pcmFormat: playbackFormat, frameCapacity: AVAudioFrameCount(frames)),
              let channel = buffer.floatChannelData?[0]
        else { return }

        buffer.frameLength = AVAudioFrameCount(frames)
        pcm.withUnsafeBytes { raw in
            let samples = raw.bindMemory(to: Int16.self)
            for index in 0..<frames {
                channel[index] = Float(Int16(littleEndian: samples[index])) / 32768.0
            }
        }

        player.scheduleBuffer(buffer, completionHandler: nil)
    }

    /// Drops audio that has not been played yet. Called when the speaker interrupts, because
    /// already-buffered speech would otherwise keep playing over them.
    public func flushPlayback() {
        guard state.isRunning else { return }
        player.stop()
        player.play()
    }

    private func configureAudioSession() throws {
        #if os(iOS) || os(visionOS)
        let session = AVAudioSession.sharedInstance()
        do {
            // `.voiceChat` is what turns on the system echo canceller, noise suppression, and gain
            // control. Any other mode and the agent hears its own voice.
            try session.setCategory(
                .playAndRecord,
                mode: .voiceChat,
                options: configuration.preferSpeaker ? [.defaultToSpeaker, .allowBluetooth] : [.allowBluetooth]
            )
            try session.setPreferredSampleRate(configuration.sampleRate)
            try session.setPreferredIOBufferDuration(Double(configuration.chunkFrames) / configuration.sampleRate)
            try session.setActive(true, options: [])
            if configuration.preferSpeaker, session.currentRoute.outputs.allSatisfy({ $0.portType == .builtInReceiver }) {
                try session.overrideOutputAudioPort(.speaker)
            }
        } catch {
            throw AudioError.sessionConfiguration(error.localizedDescription)
        }
        #endif
    }

    private func observeInterruptions() {
        #if os(iOS) || os(visionOS)
        guard state.observers.isEmpty else { return }
        let center = NotificationCenter.default

        let interruption = center.addObserver(
            forName: AVAudioSession.interruptionNotification,
            object: nil,
            queue: .main
        ) { [weak self] notification in
            guard
                let self,
                let raw = notification.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt,
                let type = AVAudioSession.InterruptionType(rawValue: raw)
            else { return }
            switch type {
            case .began:
                self.onInterruption?(true)
            case .ended:
                try? AVAudioSession.sharedInstance().setActive(true)
                self.onInterruption?(false)
            @unknown default:
                break
            }
        }

        // A route change can silently move capture to a device with different echo behaviour, so
        // the engine is restarted rather than left running against a stale configuration.
        let route = center.addObserver(
            forName: AVAudioSession.routeChangeNotification,
            object: nil,
            queue: .main
        ) { [weak self] _ in
            guard let self, self.state.isRunning, !self.engine.isRunning else { return }
            try? self.engine.start()
        }

        state.observers = [interruption, route]
        #endif
    }

    private func removeObservers() {
        for observer in state.observers { NotificationCenter.default.removeObserver(observer) }
        state.observers = []
    }
}

/// Carries an audio buffer across a `@Sendable` boundary. `AVAudioPCMBuffer` is not `Sendable`, but
/// a tap hands ownership of its buffer to exactly one consumer for the duration of the callback.
struct Unsafe<Value>: @unchecked Sendable {
    let value: Value
    init(_ value: Value) { self.value = value }
}

/// Mutable audio state, shared between the audio thread and the caller under a lock.
final class AudioState: @unchecked Sendable {
    private let lock = NSLock()
    private var pending = Data()
    private var converter: AVAudioConverter?
    private var captureFormat: AVAudioFormat?
    private var running = false
    private var storedPlaybackFormat: AVAudioFormat?
    private var storedObservers: [NSObjectProtocol] = []

    var isRunning: Bool {
        get { lock.lock(); defer { lock.unlock() }; return running }
        set { lock.lock(); running = newValue; lock.unlock() }
    }

    var playbackFormat: AVAudioFormat? {
        lock.lock(); defer { lock.unlock() }
        return storedPlaybackFormat
    }

    var observers: [NSObjectProtocol] {
        get { lock.lock(); defer { lock.unlock() }; return storedObservers }
        set { lock.lock(); storedObservers = newValue; lock.unlock() }
    }

    func configure(capture: AVAudioFormat, playback: AVAudioFormat, converter: AVAudioConverter) {
        lock.lock(); defer { lock.unlock() }
        self.captureFormat = capture
        self.storedPlaybackFormat = playback
        self.converter = converter
        self.pending = Data()
    }

    func reset() {
        lock.lock(); defer { lock.unlock() }
        pending = Data()
        running = false
    }

    /// Resamples one hardware buffer and returns whole chunks. Bursts of uneven size make
    /// server-side endpointing jumpy, so partial bytes are held back for the next buffer.
    func convert(_ source: Unsafe<AVAudioPCMBuffer>, chunkBytes: Int) -> [Data] {
        lock.lock(); defer { lock.unlock() }
        guard let converter, let captureFormat else { return [] }

        let buffer = source.value
        let ratio = captureFormat.sampleRate / buffer.format.sampleRate
        let capacity = AVAudioFrameCount(Double(buffer.frameLength) * ratio) + 1024
        guard let converted = AVAudioPCMBuffer(pcmFormat: captureFormat, frameCapacity: capacity) else { return [] }

        // The converter calls its block repeatedly and expects nil once the source is drained.
        let pump = SourcePump(buffer)
        var error: NSError?
        converter.convert(to: converted, error: &error) { _, status in
            guard let next = pump.take() else {
                status.pointee = .noDataNow
                return nil
            }
            status.pointee = .haveData
            return next
        }

        guard error == nil, converted.frameLength > 0, let channel = converted.int16ChannelData else { return [] }

        let byteCount = Int(converted.frameLength) * MemoryLayout<Int16>.size
        channel[0].withMemoryRebound(to: UInt8.self, capacity: byteCount) { bytes in
            pending.append(bytes, count: byteCount)
        }

        var chunks: [Data] = []
        while pending.count >= chunkBytes {
            chunks.append(Data(pending.prefix(chunkBytes)))
            pending.removeFirst(chunkBytes)
        }
        return chunks
    }
}

private final class SourcePump: @unchecked Sendable {
    private var buffer: AVAudioPCMBuffer?
    init(_ buffer: AVAudioPCMBuffer) { self.buffer = buffer }
    func take() -> AVAudioPCMBuffer? {
        defer { buffer = nil }
        return buffer
    }
}
