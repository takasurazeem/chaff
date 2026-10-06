import SwiftUI
import chaff_ffiFFI

/// The chips and menus above the grid.
///
/// # Why the counts are always visible
///
/// A filter with no counts is one you press to find out what it does. "Review 2,349" answers
/// *before* the click, and — more usefully — tells a user at a glance that 562 photographs have
/// already been rejected without them having to remember.
///
/// # Why the counts do not change as you narrow
///
/// They are counted against the **unfiltered** library. A "Reject 312" that became "Reject 4"
/// once Keep was selected would be describing the filter rather than the library, and the number
/// a user is looking for is the one about the library.
struct FilterBar: View {
    @Binding var filters: Filters
    let photos: [Photo]

    private var bands: [Filters.Band: Int] { Filters.bandCounts(photos) }
    private var decisions: [Filters.Decision: Int] { Filters.decisionCounts(photos) }
    private var facets: (cameras: [String], lenses: [String], years: [Int32]) {
        Filters.facets(photos)
    }
    private var qualityCounts: [Filters.Quality: Int] { Filters.qualityCounts(photos) }

    var body: some View {
        // **Scrollable, because the filters do not fit and must not pretend to.**
        //
        // The first version was a bare `HStack`, and with fourteen chips and three menus in a
        // window that is often narrower than that, SwiftUI compressed every `Text` to its minimum
        // width — which wraps one character per line. "Engine" became a vertical column of six
        // letters, and the whole bar was unreadable.
        //
        // A layout that silently destroys its content is worse than one that overflows: overflow
        // is visible, and a scroll is the obvious answer to it.
        ScrollView(.horizontal) {
            HStack(spacing: 10) {
            // **Two groups, labelled.** "Keep 45" and "Rated 12" are different questions — what
            // the engine thinks and what the user decided — and unlabelled chips side by side
            // read as one list where the numbers contradict each other.
            group("Engine", role: "Filter by what the engine decided") {
                ForEach(Filters.Band.allCases) { band in
                    Chip(
                        label: band.label,
                        count: bands[band] ?? 0,
                        active: filters.band == band,
                        tone: tone(band)
                    ) { filters.band = band }
                }
            }

            group("Yours", role: "Filter by what you decided") {
                ForEach(Filters.Decision.allCases) { decision in
                    Chip(
                        label: decision.label,
                        count: decisions[decision] ?? 0,
                        active: filters.decision == decision,
                        tone: decision == .rejected ? .reject : .neutral
                    ) { filters.decision = decision }
                }
            }

            // **Quality, which is measured rather than guessed.**
            //
            // The engine has always measured sharpness and noise; this is the first way to act on
            // it. The counts come from percentiles within the shoot, which is what makes them
            // comparable at all — an absolute sharpness number means nothing without knowing the
            // lens, the subject and the light.
            group("Quality", role: "Filter by measured sharpness and noise") {
                ForEach(Filters.Quality.allCases) { q in
                    Chip(
                        label: q.label,
                        count: qualityCounts[q] ?? 0,
                        active: filters.quality == q,
                        tone: q == .soft || q == .noisy || q == .lowDetail ? .warn : .keep
                    ) {
                        // Tapping the active chip clears it — the same toggle the band chips do
                        // not have, because these are a single choice among four rather than a
                        // view onto a partition.
                        filters.quality = filters.quality == q ? nil : q
                    }
                    .help(q.help)
                }
            }

            Spacer(minLength: 8)

            // **Hidden when the library has none.** A "Camera" menu with no cameras in it suggests
            // a bug, and a library of one camera does not need the control at all.
            if facets.cameras.count > 1 {
                facet("Camera", options: facets.cameras, selection: $filters.camera)
            }
            if facets.lenses.count > 1 {
                facet("Lens", options: facets.lenses, selection: $filters.lens)
            }
            if facets.years.count > 1 {
                yearFacet
            }

                if filters.isActive {
                    Button("Clear") { filters = Filters() }
                        .buttonStyle(.plain)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .fixedSize()
                        .help("Show every photograph again")
                }
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 6)
        }
        .scrollIndicators(.hidden)
    }

    @ViewBuilder
    private func group<Content: View>(
        _ title: String,
        role: String,
        @ViewBuilder content: () -> Content
    ) -> some View {
        HStack(spacing: 4) {
            Text(title)
                .font(.caption2)
                .foregroundStyle(.tertiary)
                .fixedSize()
                .accessibilityHidden(true)
            content()
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel(title)
        .accessibilityHint(role)
    }

    private func facet(
        _ title: String,
        options: [String],
        selection: Binding<String?>
    ) -> some View {
        Picker(title, selection: selection) {
            Text("All \(title.lowercased())s").tag(String?.none)
            Divider()
            ForEach(options, id: \.self) { option in
                Text(option).tag(String?.some(option))
            }
        }
        .labelsHidden()
        // `fixedSize` before the frame: a `Picker` with a `maxWidth` and no floor will shrink to
        // nothing rather than overflow, and an unreadable menu is worse than a wide one.
        .fixedSize()
        .frame(maxWidth: 190)
        .accessibilityLabel(title)
    }

    private var yearFacet: some View {
        Picker("Year", selection: $filters.year) {
            Text("All years").tag(Int32?.none)
            Divider()
            ForEach(facets.years, id: \.self) { year in
                Text(String(year)).tag(Int32?.some(year))
            }
        }
        .labelsHidden()
        .fixedSize()
        .frame(maxWidth: 130)
        .accessibilityLabel("Year")
    }

    private func tone(_ band: Filters.Band) -> Chip.Tone {
        switch band {
        case .keep: .keep
        case .reject: .reject
        default: .neutral
        }
    }
}

/// One filter, with its count.
struct Chip: View {
    /// `warn` is for a chip that describes a problem — soft, noisy, low detail. Amber rather
    /// than red: a soft photograph is not an error, and colouring it like a rejection would
    /// suggest the application had decided something it has not.
    enum Tone { case neutral, keep, reject, warn }

    let label: String
    let count: Int
    let active: Bool
    let tone: Tone
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: 4) {
                Text(label)
                Text("\(count)").monospacedDigit().opacity(0.7)
            }
            .font(.caption)
            .padding(.horizontal, 8)
            .padding(.vertical, 3)
            .background(background, in: Capsule())
            .foregroundStyle(foreground)
        }
        .buttonStyle(.plain)
        // **Never compress.** A chip is a word and a number; it has no state in which showing
        // half of each is useful, and without this SwiftUI will shrink it to fit whatever space
        // is left — which is how "Reject" became a vertical column of six letters.
        .fixedSize()
        // **The count is part of the label, not decoration.** A screen reader hearing "Reject"
        // and not "Reject, 312" loses the number that makes the chip worth reading.
        .accessibilityLabel("\(label), \(count) photographs")
        .accessibilityAddTraits(active ? [.isSelected] : [])
    }

    private var background: AnyShapeStyle {
        guard active else { return AnyShapeStyle(.quaternary) }
        switch tone {
        case .neutral: return AnyShapeStyle(.tint.opacity(0.25))
        case .keep: return AnyShapeStyle(Color.green.opacity(0.25))
        case .reject: return AnyShapeStyle(Color.red.opacity(0.25))
        case .warn: return AnyShapeStyle(Color.orange.opacity(0.25))
        }
    }

    private var foreground: AnyShapeStyle {
        guard active else { return AnyShapeStyle(.secondary) }
        switch tone {
        case .neutral: return AnyShapeStyle(.primary)
        case .keep: return AnyShapeStyle(Color.green)
        case .reject: return AnyShapeStyle(Color.red)
        case .warn: return AnyShapeStyle(Color.orange)
        }
    }
}
