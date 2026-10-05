import Testing
@testable import Chaff

/// The culling state machine.
///
/// `Culling` holds the cursor and the undo stack. The engine calls inside `rate` and `undo` are
/// not exercised here — those need an `EngineModel`, which needs a catalog, and a test that
/// builds one is testing the engine rather than this type. What *is* testable without any of
/// that is the state the type owns, and that is where the bugs that eat an afternoon live.
@MainActor
struct CullingTests {
    @Test("The undo depth starts at zero and reports what it holds")
    func undoStartsEmpty() {
        let culling = Culling()
        #expect(culling.undoDepth == 0)
        #expect(culling.cursor == nil)
    }

    @Test("Recording a move that moved nothing records nothing")
    func anEmptyMoveIsNotUndoable() {
        // A delete that moved zero files has nothing to reverse, and pushing it would make ⌘Z
        // do nothing visible — which reads as undo being broken rather than as there being
        // nothing to undo.
        let culling = Culling()
        culling.recordTrash(opId: "op-1", moved: 0)
        #expect(culling.undoDepth == 0, "a move of zero files must not become an undo step")
    }

    @Test("Recording a real move adds exactly one step, whatever the file count")
    func oneMoveIsOneStep() {
        // **One operation is one undo**, not one per file. Ten steps for one delete means ⌘Z
        // ten times to reverse a single action.
        let culling = Culling()
        culling.recordTrash(opId: "op-1", moved: 1)
        #expect(culling.undoDepth == 1)
        culling.recordTrash(opId: "op-2", moved: 47)
        #expect(culling.undoDepth == 2)
    }

    @Test("The undo stack is bounded, so a long session does not grow forever")
    func undoIsBounded() {
        // The web app bounds its stack at 500 for the same reason. A culling session is
        // thousands of keystrokes, and an unbounded stack is a leak that only shows on the
        // session that matters.
        let culling = Culling()
        for i in 0..<700 {
            culling.recordTrash(opId: "op-\(i)", moved: 1)
        }
        #expect(culling.undoDepth == 500, "the stack must be capped at its limit")
    }

    @Test("The cursor is independent of anything else the type holds")
    func theCursorIsItsOwnState() {
        // The cursor is what `3` rates and the selection is what a delete would move. They are
        // deliberately separate — collapsing them means rating four photographs at once — and
        // this asserts the cursor is settable on its own.
        let culling = Culling()
        culling.cursor = 42
        #expect(culling.cursor == 42)
        culling.cursor = nil
        #expect(culling.cursor == nil)
    }
}
