import SwiftUI

/// The comparison, with the grid's visible list supplied.
///
/// A thin wrapper because the sheet is presented from the app while the list of what the grid is
/// showing lives in the library view. `Compare` takes it as a parameter so it can page through
/// **what the user was looking at** rather than the whole library.
struct CompareSheet: View {
    @Environment(EngineModel.self) private var model

    var body: some View {
        // `@Bindable` for the binding: `@Environment` hands back the object, and a `Binding`
        // needs the wrapper.
        @Bindable var model = model

        return Compare(photos: model.photos, isPresented: $model.showCompare)
    }
}
