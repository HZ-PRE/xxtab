import AppKit
let bitmap = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: 1024, pixelsHigh: 1024,
    bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
NSGraphicsContext.saveGraphicsState()
NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: bitmap)
NSColor(red: 0.078, green: 0.388, blue: 0.839, alpha: 1).setFill()
NSBezierPath(roundedRect: NSRect(x: 32, y: 32, width: 960, height: 960), xRadius: 220, yRadius: 220).fill()
NSColor.white.setStroke()
for points in [[NSPoint(x: 320, y: 320), NSPoint(x: 704, y: 704)], [NSPoint(x: 704, y: 320), NSPoint(x: 320, y: 704)]] {
    let path = NSBezierPath(); path.lineWidth = 128; path.lineCapStyle = .round
    path.move(to: points[0]); path.line(to: points[1]); path.stroke()
}
NSGraphicsContext.restoreGraphicsState()
try bitmap.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: CommandLine.arguments[1]))
