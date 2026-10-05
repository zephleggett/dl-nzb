import SwiftUI

/// The widest one column of rows grows: about what UIKit's readable content
/// guide allows. Wider, and on an iPad a row's name and its control end up a
/// screen apart.
let readableContentWidth: CGFloat = 672

extension View {
  /// A list or form whose rows stay within `readableContentWidth`, centred,
  /// on a screen wider than that; the system's own margins on a narrower one.
  func readableWidth() -> some View {
    modifier(ReadableWidth())
  }
}

private struct ReadableWidth: ViewModifier {
  @State private var margins = ReadableMargins()

  func body(content: Content) -> some View {
    content
      .contentMargins(.leading, margins.leading, for: .scrollContent)
      .contentMargins(.trailing, margins.trailing, for: .scrollContent)
      .onGeometryChange(for: ReadableMargins.self) {
        ReadableMargins(width: $0.size.width, safeArea: $0.safeAreaInsets)
      } action: {
        margins = $0
      }
  }
}

/// The scroll view's side margins that centre `readableContentWidth` in the
/// part of the screen the list has to itself.
///
/// A list's content margins count from the scroll view's own edges, and the
/// scroll view runs on under whatever covers its sides: the iPad's floating
/// sidebar, an iPhone's sensor housing in landscape. Those are its safe area;
/// the view's size is what is left. So each side's margin is that side's
/// safe area plus half the room to spare.
struct ReadableMargins: Equatable {
  /// Nil keeps the system's margin until centring needs more than it.
  var leading: CGFloat?
  var trailing: CGFloat?

  init(leading: CGFloat? = nil, trailing: CGFloat? = nil) {
    self.leading = leading
    self.trailing = trailing
  }

  init(width: CGFloat, safeArea: EdgeInsets, readable: CGFloat = readableContentWidth) {
    let side = (width - readable) / 2
    guard side > 20 else {
      self.init()
      return
    }
    self.init(leading: safeArea.leading + side, trailing: safeArea.trailing + side)
  }
}
