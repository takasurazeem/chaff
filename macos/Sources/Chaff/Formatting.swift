import Foundation

/// How a value is written for a person to read.
///
/// **Its own file, beside `FormattingTests`.** These are the four functions a user reads on every
/// photograph, they are pure, and they were the reason `Inspector.swift` ran to 231 lines. The
/// `swiftui-pro` review's rule is that a file holds one type; the rule's real point is that a
/// reader should be able to find the thing they came for.
///
/// Free functions rather than view methods, so they are testable without a view — the web app
/// learned that when the same helpers had to move out of a component.

//
// Free functions rather than view methods so they are testable without a view — the web app
// learned that lesson when the same helpers had to move out of a component to satisfy a lint
// rule about fast refresh.

/// `1/500` rather than `0.002`. Every camera and every editor writes it the first way.
func formatShutter(_ seconds: Double?) -> String? {
    guard let seconds, seconds > 0 else { return nil }
    if seconds >= 1 { return String(format: "%.1f s", seconds) }
    // **The unit belongs on both branches.** The first version put ` s` only on the seconds
    // branch, so a shutter read `1/500` while a long exposure read `2.0 s` — two formats for
    // one quantity, and the web app's version says `1/500 s`. A test caught it.
    return "1/\(Int((1 / seconds).rounded())) s"
}

func formatAperture(_ f: Double?) -> String? {
    guard let f, f > 0 else { return nil }
    return f == f.rounded() ? "f/\(Int(f))" : String(format: "f/%.1f", f)
}

/// Rounded, because nobody shoots 24.7 mm.
func formatFocal(_ mm: Double?) -> String? {
    guard let mm, mm > 0 else { return nil }
    return "\(Int(mm.rounded())) mm"
}

/// Labelled as the camera's clock, because that is what it is.
func formatCaptured(_ epoch: Int64?) -> String? {
    guard let epoch else { return nil }
    let date = Date(timeIntervalSince1970: TimeInterval(epoch))
    let formatter = DateFormatter()
    formatter.dateFormat = "yyyy-MM-dd HH:mm"
    formatter.timeZone = TimeZone(secondsFromGMT: 0)
    return "\(formatter.string(from: date)) (camera clock)"
}
