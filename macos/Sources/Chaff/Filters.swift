import Foundation
import chaff_ffiFFI

/// What the grid is narrowed to, beyond the folder and the tag.
///
/// # Why this is a value with a `matches`, and not conditions in a view
///
/// The same reason `Narrowing` is: a filter that matches nothing produces an **empty grid with no
/// error and no clue**, and the folder comparison beside this one was silently broken for a while
/// — a relative path against an absolute one, showing nothing for a folder labelled 2,071
/// photographs.
///
/// # Why client-side
///
/// Every field this reads is already on `Photo`: the engine sends `band`, `rating`, `rejected`,
/// `camera`, `lens` and `year` with the list. Filtering in Swift is instant and needs no round
/// trip, which matters because a filter is adjusted while looking at the result — a query per
/// keystroke would make the grid lag behind the control.
struct Filters: Equatable {
    /// The engine's own verdict.
    enum Band: String, CaseIterable, Identifiable {
        case all, keep, review, reject
        var id: String { rawValue }
        var label: String { rawValue.capitalized }
    }

    /// What the user decided.
    enum Decision: String, CaseIterable, Identifiable {
        case all, unrated, rated, rejected
        var id: String { rawValue }
        var label: String { rawValue.capitalized }
    }

    /// **A quality signal, derived from a measurement rather than from a model.**
    ///
    /// The engine has measured sharpness and noise since the beginning — `scoring/focus.rs` is a
    /// real blur metric, built so a shallow-depth-of-field portrait is *not* marked blurry. What
    /// was missing was any way to act on it: the numbers appeared in the inspector, one
    /// photograph at a time, and a library of 3,000 could not be narrowed to the soft ones.
    ///
    /// **The thresholds are percentiles within the shoot**, which is what makes them comparable
    /// at all — an absolute sharpness number means nothing without knowing the lens, the subject
    /// and the light. "Softer than 90% of the frames taken alongside it" is a statement a user can
    /// act on.
    enum Quality: String, CaseIterable, Identifiable {
        /// The softest tenth of the shoot.
        case soft
        /// The noisiest tenth — the one metric where *more* is worse.
        case noisy
        /// The least detailed tenth: soft, or the subject is small in frame.
        case lowDetail
        /// The sharpest quarter, which is where a keeper usually comes from.
        case sharp

        var id: String { rawValue }

        var label: String {
            switch self {
            case .soft: "Soft"
            case .noisy: "Noisy"
            case .lowDetail: "Low detail"
            case .sharp: "Sharp"
            }
        }

        var help: String {
            switch self {
            case .soft: "The softest tenth of this shoot"
            case .noisy: "The noisiest tenth of this shoot"
            case .lowDetail: "The least detailed tenth of this shoot"
            case .sharp: "The sharpest quarter of this shoot"
            }
        }
    }

    var band: Band = .all
    /// `nil` is every photograph. One value rather than a set, because the four are not
    /// independent — "soft" and "sharp" together is a contradiction, and a UI that allowed it
    /// would show an empty grid for a reason nobody could see.
    var quality: Quality?
    var decision: Decision = .all
    /// `nil` means every camera — **not the empty string**, which would be a camera named "".
    var camera: String?
    var lens: String?
    var year: Int32?

    var isActive: Bool {
        band != .all || decision != .all || quality != nil
            || camera != nil || lens != nil || year != nil
    }

    /// How many photographs each quality signal holds, for the chips' counts.
    ///
    /// Counted against the **unfiltered** list, like the band counts, so the numbers describe the
    /// library rather than the filter.
    static func qualityCounts(_ photos: [Photo]) -> [Quality: Int] {
        var out: [Quality: Int] = [:]
        for q in Quality.allCases {
            out[q] = photos.count { photo in
                switch q {
                case .soft: (photo.focus ?? 101) <= 10
                case .noisy: (photo.noise ?? -1) >= 90
                case .lowDetail: (photo.detail ?? 101) <= 10
                case .sharp: (photo.focus ?? -1) >= 75
                }
            }
        }
        return out
    }

    /// Does a photograph survive the filter?
    func matches(_ photo: Photo) -> Bool {
        // **The engine's band.** A photograph with no band is one that was not scored — a raw
        // this build cannot decode — and it belongs in "All" and in none of the three.
        switch band {
        case .all: break
        case .keep: if photo.band != "keep" { return false }
        case .review: if photo.band != "review" { return false }
        case .reject: if photo.band != "reject" { return false }
        }

        // **The user's decision**, which is a different question from the engine's.
        switch decision {
        case .all: break
        case .unrated: if photo.rating > 0 || photo.rejected { return false }
        case .rated: if photo.rating == 0 { return false }
        case .rejected: if !photo.rejected { return false }
        }

        // **Absent is not zero.** A photograph with no focus score is one the engine could not
        // measure — a raw this build cannot decode — and it belongs in none of these. Treating it
        // as `0` would file every unreadable raw under "Soft", which is a claim nobody made.
        if let quality {
            switch quality {
            case .soft:
                guard let f = photo.focus, f <= 10 else { return false }
            case .noisy:
                guard let n = photo.noise, n >= 90 else { return false }
            case .lowDetail:
                guard let d = photo.detail, d <= 10 else { return false }
            case .sharp:
                guard let f = photo.focus, f >= 75 else { return false }
            }
        }

        if let camera, photo.camera != camera { return false }
        if let lens, photo.lens != lens { return false }
        if let year, photo.year != year { return false }
        return true
    }

    /// How many photographs each band holds, for the chips' counts.
    ///
    /// **Counted against the unfiltered list**, so the numbers do not change as the user narrows —
    /// a "Reject 312" that became "Reject 4" once Keep was selected would be describing the
    /// filter rather than the library.
    static func bandCounts(_ photos: [Photo]) -> [Band: Int] {
        var out: [Band: Int] = [.all: photos.count, .keep: 0, .review: 0, .reject: 0]
        for photo in photos {
            switch photo.band {
            case "keep": out[.keep, default: 0] += 1
            case "review": out[.review, default: 0] += 1
            case "reject": out[.reject, default: 0] += 1
            default: break
            }
        }
        return out
    }

    static func decisionCounts(_ photos: [Photo]) -> [Decision: Int] {
        var out: [Decision: Int] = [.all: photos.count, .unrated: 0, .rated: 0, .rejected: 0]
        for photo in photos {
            if photo.rejected { out[.rejected, default: 0] += 1 }
            if photo.rating > 0 { out[.rated, default: 0] += 1 }
            if photo.rating == 0 && !photo.rejected { out[.unrated, default: 0] += 1 }
        }
        return out
    }

    /// The cameras, lenses and years this library actually contains.
    ///
    /// **Built from the library rather than hard-coded**, and a dropdown with nothing in it is
    /// hidden rather than shown empty — a "Camera" menu with no cameras suggests a bug.
    static func facets(_ photos: [Photo]) -> (cameras: [String], lenses: [String], years: [Int32]) {
        var cameras = Set<String>()
        var lenses = Set<String>()
        var years = Set<Int32>()
        for photo in photos {
            if let c = photo.camera, !c.isEmpty { cameras.insert(c) }
            if let l = photo.lens, !l.isEmpty { lenses.insert(l) }
            // **Optional, because a file with no capture date has no year** — a stripped JPEG,
            // a scan. `0` is not a year and must not become a menu entry.
            if let y = photo.year, y > 0 { years.insert(y) }
        }
        return (cameras.sorted(), lenses.sorted(), years.sorted(by: >))
    }
}
