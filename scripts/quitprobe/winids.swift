// winids <pid> — prints "windowID title" for each on-screen window of pid
import CoreGraphics
import Foundation
let pid = Int32(CommandLine.arguments[1])!
let list = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as! [[String: Any]]
for w in list where (w["kCGWindowOwnerPID"] as? Int32) == pid {
    let id = w["kCGWindowNumber"] as! Int
    let title = (w["kCGWindowName"] as? String) ?? ""
    let layer = w["kCGWindowLayer"] as! Int
    print("\(id) layer=\(layer) title=\(title)")
}
