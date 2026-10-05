import SwiftUI

/// Newer APIs where they exist, the older equivalent where they do not.
///
/// # Why this file exists
///
/// The first version declared `platforms: [.macOS("26.0")]` — a hard floor — because that is
/// what makes the system hand the chrome Liquid Glass. It works, and it **drops every user on
/// macOS 14 or 15** to get a material on a toolbar.
///
/// The deployment target says what you *require*. Availability says what you *prefer*. Raising
/// the target to use one modifier conflates them, and the cost lands on people who did nothing
/// wrong except not upgrade.
///
/// So the target is macOS 14 and everything newer is gated. Each helper below is one API with
/// two implementations, which keeps the `#available` checks in one file instead of scattered
/// through the views — the alternative is a dozen `if #available` branches in layout code,
/// where a missed one is a crash on an older machine rather than a compile error.
///
/// # What degrades, and how much
///
/// * **Glass** becomes `regularMaterial`. Visually different, functionally identical, and it is
///   what macOS 14 shipped for exactly this purpose.
/// * **Glass buttons** become `.bordered`. The same affordance with the older chrome.
///
/// Nothing here changes behaviour. If a helper ever needs to, it does not belong in this file.

extension View {
    /// A floating surface: Liquid Glass on macOS 26, a material before it.
    ///
    /// For cards that genuinely float over content — not for panels that are part of the layout,
    /// where a material is correct on every version.
    @ViewBuilder
    func chaffFloatingSurface(cornerRadius: CGFloat = 16) -> some View {
        if #available(macOS 26.0, *) {
            self.glassEffect(.regular, in: .rect(cornerRadius: cornerRadius))
        } else {
            self.background(.regularMaterial, in: RoundedRectangle(cornerRadius: cornerRadius))
        }
    }
}

extension Button {
    /// The platform's current button chrome, whichever that is.
    @ViewBuilder
    func chaffFloatingButton() -> some View {
        if #available(macOS 26.0, *) {
            self.buttonStyle(.glass)
        } else {
            self.buttonStyle(.bordered)
        }
    }
}
