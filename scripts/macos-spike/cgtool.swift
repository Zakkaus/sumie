// Drives the sumie runner window through the window server for U4 measurements.
// Usage:
//   cgtool front PID            activate PID, print the frontmost PID
//   cgtool bounds PID           print x y w h onscreen(0/1) of PID's layer-0 window
//   cgtool cmdm                 post Cmd+M to the HID tap
//   cgtool drag X0 Y0 X1 Y1 MS  press, drag over MS milliseconds, release
//   cgtool menuitem PID         print x y w h of the application menu's title
//   cgtool minbutton PID        print the centre of the window's minimize button
//   cgtool click X Y            press and release at X Y
//   cgtool clickown PID X Y     the same, only if PID is frontmost and owns the topmost window there
//   cgtool hold X Y MS RX RY    press at X Y, hold MS, move to RX RY, release
//   cgtool cmdq PID             post Cmd+Q to the HID tap if PID is frontmost
//   cgtool axquit PID           press the application menu's Quit item through AX
//   cgtool closebutton PID      print the centre of the window's close button
//   cgtool dockquit PID         choose Quit from PID's Dock item menu through AX
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

func attribute(_ element: AXUIElement, _ name: String) -> CFTypeRef? {
    var value: CFTypeRef?
    return AXUIElementCopyAttributeValue(element, name as CFString, &value) == .success ? value : nil
}

func children(_ element: AXUIElement) -> [AXUIElement] {
    (attribute(element, kAXChildrenAttribute) as? [AXUIElement]) ?? []
}

func title(_ element: AXUIElement) -> String {
    (attribute(element, kAXTitleAttribute) as? String) ?? ""
}

func centre(_ element: AXUIElement) -> CGPoint? {
    guard let position = attribute(element, kAXPositionAttribute), let size = attribute(element, kAXSizeAttribute)
    else { return nil }
    var p = CGPoint.zero, s = CGSize.zero
    AXValueGetValue(position as! AXValue, .cgPoint, &p)
    AXValueGetValue(size as! AXValue, .cgSize, &s)
    return CGPoint(x: p.x + s.width / 2, y: p.y + s.height / 2)
}

/// The first menu item below `element` whose title passes `matches`, searched depth first.
func menuItem(_ element: AXUIElement, _ matches: (String) -> Bool) -> AXUIElement? {
    for child in children(element) {
        if (attribute(child, kAXRoleAttribute) as? String) == kAXMenuItemRole && matches(title(child)) {
            return child
        }
        if let found = menuItem(child, matches) { return found }
    }
    return nil
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
case "clickown":
    let (pid, point) = (Int32(args[2])!, CGPoint(x: Double(args[3])!, y: Double(args[4])!))
    let list = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as! [[String: Any]]
    let dock = NSRunningApplication.runningApplications(withBundleIdentifier: "com.apple.dock").first?.processIdentifier
    // The Dock's full-screen layer and transparent windows let clicks through.
    let top = list.first { info in
        guard let b = info[kCGWindowBounds as String] as? [String: Double],
              (info[kCGWindowOwnerPID as String] as? Int32) != dock,
              ((info[kCGWindowAlpha as String] as? Double) ?? 1) > 0 else { return false }
        return CGRect(x: b["X"]!, y: b["Y"]!, width: b["Width"]!, height: b["Height"]!).contains(point)
    }
    // Never click another application's window.
    guard NSWorkspace.shared.frontmostApplication?.processIdentifier == pid,
          (top?[kCGWindowOwnerPID as String] as? Int32) == pid else {
        print("covered"); exit(1)
    }
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
case "cmdq":
    // Only ever quit the probe: refuse unless it is the frontmost application.
    guard NSWorkspace.shared.frontmostApplication?.processIdentifier == Int32(args[2])! else {
        print("not frontmost"); exit(1)
    }
    for down in [true, false] {
        let event = CGEvent(keyboardEventSource: nil, virtualKey: 12, keyDown: down)!
        event.flags = .maskCommand
        event.post(tap: .cghidEventTap)
        usleep(20_000)
    }
case "axquit":
    let app = AXUIElementCreateApplication(Int32(args[2])!)
    guard let bar = attribute(app, kAXMenuBarAttribute), let menu = children(bar as! AXUIElement).dropFirst().first,
          let item = menuItem(menu, { $0.hasPrefix("Quit") }) else {
        print("no Quit item (trusted: \(AXIsProcessTrusted()))"); exit(1)
    }
    exit(AXUIElementPerformAction(item, kAXPressAction as CFString) == .success ? 0 : 1)
case "closebutton":
    let app = AXUIElementCreateApplication(Int32(args[2])!)
    guard let window = (attribute(app, kAXWindowsAttribute) as? [AXUIElement])?.first,
          let button = attribute(window, kAXCloseButtonAttribute), let point = centre(button as! AXUIElement) else {
        print("none"); exit(1)
    }
    print(Int(point.x), Int(point.y))
case "dockquit":
    let pid = Int32(args[2])!
    guard let name = NSRunningApplication(processIdentifier: pid)?.localizedName,
          let dock = NSRunningApplication.runningApplications(withBundleIdentifier: "com.apple.dock").first else {
        print("no Dock or application"); exit(1)
    }
    // The Dock titles its items in the user's language, from its own strings table.
    let bundle = Bundle(url: dock.bundleURL!)!
    let language = Bundle.preferredLocalizations(from: bundle.localizations, forPreferences: Locale.preferredLanguages)
    let table = bundle.path(forResource: "DockMenus", ofType: "strings", inDirectory: nil, forLocalization: language.first)
    let quit = table.flatMap { NSDictionary(contentsOfFile: $0)?["QUIT"] as? String } ?? "Quit"
    let tiles = children(AXUIElementCreateApplication(dock.processIdentifier)).flatMap(children)
    guard let tile = tiles.first(where: { title($0) == name }) else { print("no Dock item \(name)"); exit(1) }
    AXUIElementPerformAction(tile, kAXShowMenuAction as CFString)
    usleep(500_000)
    guard let item = menuItem(tile, { $0 == quit }) else {
        AXUIElementPerformAction(tile, kAXCancelAction as CFString)
        print("no Quit in the Dock menu"); exit(1)
    }
    exit(AXUIElementPerformAction(item, kAXPressAction as CFString) == .success ? 0 : 1)
default:
    exit(2)
}
