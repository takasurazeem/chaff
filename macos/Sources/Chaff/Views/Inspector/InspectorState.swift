import chaff_ffiFFI

/// What the inspector is doing, as a value.
///
/// **Four states, not three.** The first version had `detail`, `loadFailed`, and "everything
/// else" — and "everything else" was both *loading* and *nothing selected*. Those are different:
/// one is work in progress and the other is an empty panel, and showing a spinner for the second
/// says "working on it" when there is nothing to work on. A user saw a spinner in an empty
/// inspector and reasonably read it as stuck.
///
/// Its own file so the states are readable in one place, without the layout between them.
enum InspectorState {
    case nothingSelected
    case loading
    case failed
    case loaded(PhotoDetail)

    /// The state for a given selection and load result.
    static func of(photoId: Int64?, detail: PhotoDetail?, loadFailed: Bool) -> InspectorState {
        if photoId == nil { return .nothingSelected }
        if loadFailed { return .failed }
        if let detail { return .loaded(detail) }
        return .loading
    }
}
