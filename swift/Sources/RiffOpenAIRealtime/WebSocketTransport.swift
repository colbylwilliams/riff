import Foundation
import RiffCore

/// Anything that can carry realtime events. The WebSocket implementation below is the one that
/// needs no media stack; a WebRTC transport plugs in here without the provider changing.
public protocol RiffTransport: Sendable {
    func send(_ message: JSONValue) throws
    func sendAudio(_ pcm: Data)
    func close(reason: String?) async
}

/// WebSocket transport over `URLSessionWebSocketTask`, where audio is base64 PCM inside JSON events.
///
/// WebRTC handles packet loss and jitter better on a phone, but it needs a media stack. This
/// transport needs nothing beyond Foundation, which makes it the one that works everywhere.
public final class WebSocketTransport: RiffTransport, @unchecked Sendable {
    private let task: URLSessionWebSocketTask
    private let onMessage: @Sendable (JSONValue) -> Void
    private let onClose: @Sendable (String?) -> Void
    private let onError: @Sendable (Error) -> Void
    private let lock = NSLock()
    private var closed = false

    public static let defaultURL = URL(string: "wss://api.openai.com/v1/realtime")!

    public init(
        url: URL = WebSocketTransport.defaultURL,
        model: String,
        token: String,
        organization: String? = nil,
        project: String? = nil,
        urlSession: URLSession = .shared,
        onMessage: @escaping @Sendable (JSONValue) -> Void,
        onClose: @escaping @Sendable (String?) -> Void,
        onError: @escaping @Sendable (Error) -> Void
    ) {
        self.task = urlSession.webSocketTask(with: WebSocketTransport.request(url: url, model: model, protocols: {
            var protocols = ["realtime", "openai-insecure-api-key.\(token)"]
            if let organization { protocols.append("openai-organization.\(organization)") }
            if let project { protocols.append("openai-project.\(project)") }
            return protocols
        }()))

        self.onMessage = onMessage
        self.onClose = onClose
        self.onError = onError
    }

    /// Adds the model without disturbing parameters the caller's endpoint already carries.
    ///
    /// Azure and gateway endpoints require parameters such as `api-version` and `deployment`.
    /// Replacing the query wholesale drops them, which fails in a way that looks like an auth error.
    static func request(url: URL, model: String, protocols: [String]) -> URLRequest {
        var components = URLComponents(url: url, resolvingAgainstBaseURL: false)
        var items = components?.queryItems ?? []
        items.removeAll { $0.name == "model" }
        items.append(URLQueryItem(name: "model", value: model))
        components?.queryItems = items

        var request = URLRequest(url: components?.url ?? url)
        request.setValue(protocols.joined(separator: ", "), forHTTPHeaderField: "Sec-WebSocket-Protocol")
        return request
    }

    public func resume() {
        task.resume()
        receive()
    }

    private func receive() {
        task.receive { [weak self] result in
            guard let self else { return }
            switch result {
            case .success(let message):
                let text: String? = switch message {
                case .string(let value): value
                case .data(let data): String(data: data, encoding: .utf8)
                @unknown default: nil
                }
                if let text, let data = text.data(using: .utf8),
                   let value = try? JSONDecoder().decode(JSONValue.self, from: data) {
                    self.onMessage(value)
                }
                self.receive()

            case .failure(let error):
                self.lock.lock()
                let wasClosed = self.closed
                self.lock.unlock()
                if !wasClosed {
                    self.onError(error)
                    self.onClose(error.localizedDescription)
                }
            }
        }
    }

    public func send(_ message: JSONValue) throws {
        task.send(.string(message.serialized())) { [weak self] error in
            if let error { self?.onError(error) }
        }
    }

    public func sendAudio(_ pcm: Data) {
        try? send(.object([
            "type": .string(OpenAIClientEvent.appendAudio),
            "audio": .string(pcm.base64EncodedString()),
        ]))
    }

    public func close(reason: String?) async {
        markClosed()
        task.cancel(with: .goingAway, reason: reason?.data(using: .utf8))
        onClose(reason)
    }

    // NSLock cannot be taken from an async context, so the critical section stays synchronous.
    private func markClosed() {
        lock.lock()
        closed = true
        lock.unlock()
    }
}
