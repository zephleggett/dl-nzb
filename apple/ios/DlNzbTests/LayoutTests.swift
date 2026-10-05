import SwiftUI
import Testing

@testable import DlNzbApp

@Suite("Readable width")
struct LayoutTests {
  @Test("Beside an iPad's floating sidebar, rows centre in the part of the screen left to them")
  func besideSidebar() {
    // Landscape: the detail column is 946 points wide, after a sidebar of 430.
    let margins = ReadableMargins(width: 946, safeArea: EdgeInsets(top: 0, leading: 430, bottom: 0, trailing: 0))
    #expect(margins.leading == 567)
    #expect(margins.trailing == 137)
  }

  @Test("In landscape on an iPhone, the sensor housing's side counts too")
  func iPhoneLandscape() {
    let margins = ReadableMargins(width: 750, safeArea: EdgeInsets(top: 0, leading: 62, bottom: 0, trailing: 62))
    #expect(margins.leading == 101)
    #expect(margins.trailing == 101)
  }

  @Test("Narrower than the readable width and a little, the system's margins stay")
  func narrow() {
    // Portrait split view's detail, the sidebar itself, and a phone.
    #expect(ReadableMargins(width: 602, safeArea: EdgeInsets(top: 0, leading: 430, bottom: 0, trailing: 0)) == ReadableMargins())
    #expect(ReadableMargins(width: 420, safeArea: EdgeInsets()) == ReadableMargins())
    #expect(ReadableMargins(width: 402, safeArea: EdgeInsets()) == ReadableMargins())
    #expect(ReadableMargins(width: 712, safeArea: EdgeInsets()) == ReadableMargins())
  }
}
