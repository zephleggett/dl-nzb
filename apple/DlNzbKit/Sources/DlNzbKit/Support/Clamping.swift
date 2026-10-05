extension Comparable {
  /// The value, or the nearer end of `range` when it lies outside.
  public func clamped(to range: ClosedRange<Self>) -> Self {
    min(max(self, range.lowerBound), range.upperBound)
  }
}

extension Double {
  /// The value as a share, 0...1, for a bar, a ring or a percentage. Not a
  /// number (or infinite) is 0, so a bad figure never fills one.
  public var clampedFraction: Double {
    isFinite ? clamped(to: 0...1) : 0
  }
}
