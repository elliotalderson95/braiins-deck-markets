// Prints the id of the biggest window owned by the named program (default: testbed), so that
// `screencapture -l <id>` can take that window alone and never the whole screen. It prefers a
// window that is on screen, and falls back to any window the program has.
import CoreGraphics
import Foundation

let name = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "testbed"

func biggest(_ options: CGWindowListOption) -> Int? {
    let windows = CGWindowListCopyWindowInfo(options, kCGNullWindowID) as? [[String: Any]] ?? []
    var best: (Int, Double)?
    for window in windows {
        guard let owner = window[kCGWindowOwnerName as String] as? String, owner == name,
              let id = window[kCGWindowNumber as String] as? Int,
              let bounds = window[kCGWindowBounds as String] as? [String: Double] else { continue }
        let area = (bounds["Width"] ?? 0) * (bounds["Height"] ?? 0)
        if area > 10_000, area > (best?.1 ?? 0) { best = (id, area) }
    }
    return best?.0
}

if let id = biggest([.optionOnScreenOnly]) ?? biggest([.optionAll]) {
    print(id)
}
