import Foundation

/// Runs an operation with a deadline that resumes the caller even if the operation never does.
///
/// A task group cannot express this: its scope does not exit until every child finishes, and Swift
/// cancellation is cooperative, so a host call that ignores cancellation or never returns keeps the
/// group — and the caller — blocked long after the deadline passed. Racing an unstructured task
/// against a timer is what actually lets the caller give up.
///
/// The abandoned task is left running rather than waited on. That is the point, and it is why a
/// timed-out operation with a side effect must be idempotent: the caller is told it failed while
/// the work may still land.
public func withDeadline<T: Sendable>(
    milliseconds: Int,
    onTimeout: @escaping @Sendable () -> Error,
    operation: @escaping @Sendable () async throws -> T
) async throws -> T {
    guard milliseconds > 0 else { return try await operation() }

    let gate = ResumeOnce<T>()

    let work = Task {
        do { await gate.resume(.success(try await operation())) }
        catch { await gate.resume(.failure(error)) }
    }
    let timer = Task {
        try? await Task.sleep(for: .milliseconds(milliseconds))
        guard !Task.isCancelled else { return }
        await gate.resume(.failure(onTimeout()))
    }

    defer { timer.cancel() }

    do {
        return try await gate.value()
    } catch {
        // Ask the operation to stop. It may decline, which is why the caller is already free.
        work.cancel()
        throw error
    }
}

/// Delivers whichever result arrives first and ignores the rest.
private actor ResumeOnce<T: Sendable> {
    private var result: Result<T, Error>?
    private var waiter: CheckedContinuation<T, Error>?

    func resume(_ value: Result<T, Error>) {
        guard result == nil else { return }
        result = value
        if let waiter {
            self.waiter = nil
            waiter.resume(with: value)
        }
    }

    func value() async throws -> T {
        if let result { return try result.get() }
        return try await withCheckedThrowingContinuation { continuation in
            if let result {
                continuation.resume(with: result)
            } else {
                waiter = continuation
            }
        }
    }
}
