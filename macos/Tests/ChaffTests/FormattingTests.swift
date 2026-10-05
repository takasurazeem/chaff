import Foundation
import Testing
@testable import Chaff

/// The formatting helpers, which are pure functions and therefore the easiest thing here to
/// test — and the thing a user reads on every photograph.
///
/// Swift Testing rather than XCTest: `#expect` reports the expression that failed rather than
/// "XCTAssertEqual failed", and these tests are about values rather than a UI.
struct FormattingTests {
    @Test("Shutter speed reads the way a photographer writes it")
    func shutterIsAFraction() {
        // `0.002 s` is technically correct and useless. Every camera and every editor writes
        // `1/500`, and a panel that does not makes people convert in their head.
        #expect(formatShutter(1.0 / 500) == "1/500 s")
        #expect(formatShutter(1.0 / 60) == "1/60 s")
        #expect(formatShutter(2) == "2.0 s")
        #expect(formatShutter(1) == "1.0 s")
    }

    @Test("Absent and nonsensical exposure are absent, not zero")
    func shutterRejectsWhatItCannotShow() {
        // `1/0 s` is worse than nothing: it is a value the camera never recorded, printed as
        // though it had.
        #expect(formatShutter(nil) == nil)
        #expect(formatShutter(0) == nil)
        #expect(formatShutter(-1) == nil)
    }

    @Test("Aperture is written f/N")
    func apertureIsWritten() {
        #expect(formatAperture(2.8) == "f/2.8")
        #expect(formatAperture(8) == "f/8")
        #expect(formatAperture(1.4) == "f/1.4")
        #expect(formatAperture(nil) == nil)
        #expect(formatAperture(0) == nil)
    }

    @Test("Focal length is rounded, because nobody shoots 24.7 mm")
    func focalLengthRounds() {
        #expect(formatFocal(24) == "24 mm")
        #expect(formatFocal(24.7) == "25 mm")
        #expect(formatFocal(nil) == nil)
        #expect(formatFocal(0) == nil)
    }

    @Test("Capture time says which clock it is")
    func captureTimeNamesItsClock() {
        // `parse_exif_datetime` treats the camera's local wall-clock as if it were UTC.
        // Differences between photographs are correct; the absolute instant is not. A panel
        // printing a bare time claims a precision the data does not have.
        let s = formatCaptured(1_700_000_000)
        #expect(s?.contains("camera clock") == true)
        #expect(s?.hasSuffix("(camera clock)") == true)
        #expect(formatCaptured(nil) == nil)
    }

    @Test("Capture time is rendered in UTC, not the machine's zone")
    func captureTimeDoesNotShiftWithTheMachine() {
        // **The property that makes the label honest.** If this used the local time zone, the
        // same photograph would show a different time on a machine in another zone — and the
        // label "camera clock" would be a lie, because it would be *this* clock.
        let epoch: Int64 = 1_700_000_000
        let rendered = formatCaptured(epoch)
        let expected = ISO8601DateFormatter().string(from: Date(timeIntervalSince1970: TimeInterval(epoch)))
        // The date part must match UTC, whatever the machine is set to.
        #expect(rendered?.hasPrefix(String(expected.prefix(10))) == true)
    }

    @Test("Every formatter agrees that absent is absent", arguments: [
        Double?.none, 0, -1,
    ])
    func absentIsAbsentEverywhere(_ value: Double?) {
        // A table, because the failure mode is one formatter disagreeing with the others and
        // printing `f/0` or `0 mm` for a file that recorded nothing.
        #expect(formatShutter(value) == nil)
        #expect(formatAperture(value) == nil)
        #expect(formatFocal(value) == nil)
    }
}
