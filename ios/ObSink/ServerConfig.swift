import Foundation

/// The one server this build talks to. Baked in from `OBSINK_SERVER_URL` at
/// build time (XcodeGen -> `ObSinkServerURL` in Info.plist); the public server
/// when that is empty. UI tests point a build elsewhere with
/// `OBSINK_UITEST_SERVER_URL` in the launch environment. Never shown as a
/// field: self-hosters build with their own URL.
enum ServerConfig {
    static let fallbackURL = "https://obsink.spencerjireh.com"
    static let infoKey = "ObSinkServerURL"
    static let overrideEnv = "OBSINK_UITEST_SERVER_URL"

    static let defaultURL: String = resolve(
        env: ProcessInfo.processInfo.environment,
        infoValue: Bundle.main.object(forInfoDictionaryKey: infoKey) as? String
    )

    /// Env override, then the baked value, then the fallback; blanks are
    /// treated as absent. The result is canonical (no trailing slash,
    /// lowercase scheme and host) so it compares with stored entries.
    nonisolated static func resolve(env: [String: String], infoValue: String?) -> String {
        let candidates = [env[overrideEnv], infoValue, fallbackURL]
        let chosen = candidates
            .compactMap { $0?.trimmingCharacters(in: .whitespacesAndNewlines) }
            .first { !$0.isEmpty } ?? fallbackURL
        return KeychainStore.canonicalServerURL(chosen)
    }
}
