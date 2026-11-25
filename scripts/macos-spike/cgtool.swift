// Drives the sumie runner window through the window server for U4 measurements.
// Usage:
//   cgtool front PID            activate PID, print the frontmost PID
//   cgtool bounds PID           print x y w h onscreen(0/1) of PID's layer-0 window
//   cgtool cmdm                 post Cmd+M to the HID tap
//   cgtool drag X0 Y0 X1 Y1 MS  press, drag over MS milliseconds, release
//   cgtool menuitem PID         print x y w h of the application menu's title
//   cgtool minbutton PID        print the centre of the window's minimize button
//   cgtool click X Y            press and release at X Y
//   cgtool hold X Y MS RX RY    press at X Y, hold MS, move to RX RY, release
import AppKit
import ApplicationServices
import CoreGraphics
import Foundation

func post(_ type: CGEventType, _ point: CGPoint, _ button: CGMouseButton = .left) {
    CGEvent(mouseEventSource: nil, mouseType: type, mouseCursorPosition: point, mouseButton: button)!
        .post(tap: .cghidEventTap)
}

func windowInfo(_ pid: Int32) -> [String: Any]? {
    let list = CGWindowListCopyWindowInfo([.optionAll], kCGNullWindowID) as! [[String: Any]]
    return list.first { ($0[kCGWindowOwnerPID as String] as? Int32) == pid
        && ($0[kCGWindowName as String] as? String) == "sumie / CI runner probe" }
}

let args = CommandLine.arguments
switch args[1] {
case "front":
    let pid = Int32(args[2])!
    NSRunningApplication(processIdentifier: pid)?.activate(options: [.activateAllWindows])
    usleep(300_000)
    print(NSWorkspace.shared.frontmostApplication?.processIdentifier ?? -1)
case "bounds":
    guard let info = windowInfo(Int32(args[2])!), let b = info[kCGWindowBounds as String] as? [String: Double] else {
        print("none"); exit(1)
    }
    let onscreen = (info[kCGWindowIsOnscreen as String] as? Bool) ?? false
    print(Int(b["X"]!), Int(b["Y"]!), Int(b["Width"]!), Int(b["Height"]!), onscreen ? 1 : 0)
case "cmdm":
    for down in [true, false] {
        let event = CGEvent(keyboardEventSource: nil, virtualKey: 46, keyDown: down)!
        event.flags = .maskCommand
        event.post(tap: .cghidEventTap)
        usleep(20_000)
    }
case "drag":
    let (x0, y0, x1, y1, ms) = (Double(args[2])!, Double(args[3])!, Double(args[4])!, Double(args[5])!, Double(args[6])!)
    post(.mouseMoved, CGPoint(x: x0, y: y0))
    usleep(100_000)
    post(.leftMouseDown, CGPoint(x: x0, y: y0))
    let steps = max(1, Int(ms / 16))
    for step in 1...steps {
        let t = Double(step) / Double(steps)
        post(.leftMouseDragged, CGPoint(x: x0 + (x1 - x0) * t, y: y0 + (y1 - y0) * t))
        usleep(useconds_t(ms * 1000 / Double(steps)))
    }
    post(.leftMouseUp, CGPoint(x: x1, y: y1))
case "menuitem":
    let app = AXUIElementCreateApplication(Int32(args[2])!)
    var bar: CFTypeRef?
    guard AXUIElementCopyAttributeValue(app, kAXMenuBarAttribute as CFString, &bar) == .success else {
        print("no menu bar (trusted: \(AXIsProcessTrusted()))"); exit(1)
    }
    var children: CFTypeRef?
    AXUIElementCopyAttributeValue(bar as! AXUIElement, kAXChildrenAttribute as CFString, &children)
    let items = children as! [AXUIElement]
    var position: CFTypeRef?, size: CFTypeRef?
    AXUIElementCopyAttributeValue(items[1], kAXPositionAttribute as CFString, &position)
    AXUIElementCopyAttributeValue(items[1], kAXSizeAttribute as CFString, &size)
    var p = CGPoint.zero, s = CGSize.zero
    AXValueGetValue(position as! AXValue, .cgPoint, &p)
    AXValueGetValue(size as! AXValue, .cgSize, &s)
    print(Int(p.x), Int(p.y), Int(s.width), Int(s.height))
case "minbutton":
    let app = AXUIElementCreateApplication(Int32(args[2])!)
    var windows: CFTypeRef?
    AXUIElementCopyAttributeValue(app, kAXWindowsAttribute as CFString, &windows)
    guard let window = (windows as? [AXUIElement])?.first else { print("none"); exit(1) }
    var button: CFTypeRef?
    AXUIElementCopyAttributeValue(window, kAXMinimizeButtonAttribute as CFString, &button)
    var position: CFTypeRef?, size: CFTypeRef?
    AXUIElementCopyAttributeValue(button as! AXUIElement, kAXPositionAttribute as CFString, &position)
    AXUIElementCopyAttributeValue(button as! AXUIElement, kAXSizeAttribute as CFString, &size)
    var p = CGPoint.zero, s = CGSize.zero
    AXValueGetValue(position as! AXValue, .cgPoint, &p)
    AXValueGetValue(size as! AXValue, .cgSize, &s)
    print(Int(p.x + s.width / 2), Int(p.y + s.height / 2))
case "click":
    let point = CGPoint(x: Double(args[2])!, y: Double(args[3])!)
    post(.mouseMoved, point)
    usleep(100_000)
    post(.leftMouseDown, point)
    usleep(60_000)
    post(.leftMouseUp, point)
case "hold":
    let (x, y, ms, rx, ry) = (Double(args[2])!, Double(args[3])!, Double(args[4])!, Double(args[5])!, Double(args[6])!)
    post(.mouseMoved, CGPoint(x: x, y: y))
    usleep(100_000)
    post(.leftMouseDown, CGPoint(x: x, y: y))
    usleep(useconds_t(ms * 1000))
    post(.leftMouseDragged, CGPoint(x: rx, y: ry))
    usleep(50_000)
    post(.leftMouseUp, CGPoint(x: rx, y: ry))
default:
    exit(2)
}
