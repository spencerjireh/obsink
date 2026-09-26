import SwiftUI

/// The brand on iOS (DESIGN.md section 6). Surfaces and text are the system
/// semantic colours so Dynamic Type and both appearances come for free; the
/// accent (verdigris) and the text on it are the two asset-catalog colours.
enum Theme {
    /// `AccentColor`: tinted text, controls, and the primary button fill.
    static let accent = Color("AccentColor")
    /// `OnAccent`: label text on the accent fill.
    static let onAccent = Color("OnAccent")
}

/// The one primary button on a screen (`Sync now`, `Download`, `Create
/// vault`, `Sign in`, `Unlock`, `Apply resolutions`): the iOS 26 glass
/// prominent style tinted with the accent.
struct PrimaryAction: ViewModifier {
    func body(content: Content) -> some View {
        content
            .buttonStyle(.glassProminent)
            .tint(Theme.accent)
            .foregroundStyle(Theme.onAccent)
    }
}

extension View {
    func primaryAction() -> some View { modifier(PrimaryAction()) }
}
