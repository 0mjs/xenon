// Draws Xenon's app icon: a graphite element tile, "Xe" with atomic number 54.
// Usage: swift assets/icon/make-icon.swift assets/icon/xenon.png
// Uses the bundled Archivo and IBM Plex Mono fonts; colours are the Steel tokens.

import AppKit
import CoreText

let output = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "xenon.png"
let root = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
    .deletingLastPathComponent().appendingPathComponent("fonts")
for file in ["Archivo-SemiBold.ttf", "IBMPlexMono-Medium.ttf"] {
    CTFontManagerRegisterFontsForURL(root.appendingPathComponent(file) as CFURL, .process, nil)
}

let size: CGFloat = 1024
// macOS icon grid: the tile sits inside a 100px margin, with a soft drop shadow.
let tile = CGRect(x: 100, y: 100, width: 824, height: 824)
let radius: CGFloat = 185

func rgb(_ hex: UInt32, _ alpha: CGFloat = 1) -> CGColor {
    CGColor(
        red: CGFloat((hex >> 16) & 0xff) / 255,
        green: CGFloat((hex >> 8) & 0xff) / 255,
        blue: CGFloat(hex & 0xff) / 255,
        alpha: alpha)
}

let space = CGColorSpace(name: CGColorSpace.sRGB)!
let ctx = CGContext(
    data: nil, width: Int(size), height: Int(size), bitsPerComponent: 8, bytesPerRow: 0,
    space: space, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
let tilePath = CGPath(roundedRect: tile, cornerWidth: radius, cornerHeight: radius, transform: nil)

// Drop shadow.
ctx.saveGState()
ctx.setShadow(offset: CGSize(width: 0, height: -12), blur: 28, color: rgb(0x000000, 0.45))
ctx.addPath(tilePath)
ctx.setFillColor(rgb(0x0f1012))
ctx.fillPath()
ctx.restoreGState()

// Graphite body: a gentle top-to-bottom gradient.
ctx.saveGState()
ctx.addPath(tilePath)
ctx.clip()
let body = CGGradient(
    colorsSpace: space, colors: [rgb(0x23252b), rgb(0x111215)] as CFArray, locations: [0, 1])!
ctx.drawLinearGradient(
    body, start: CGPoint(x: 0, y: tile.maxY), end: CGPoint(x: 0, y: tile.minY), options: [])

// The faint glow of a xenon lamp, from the upper right.
let glow = CGGradient(
    colorsSpace: space, colors: [rgb(0x8fa3bd, 0.20), rgb(0x8fa3bd, 0)] as CFArray,
    locations: [0, 1])!
ctx.drawRadialGradient(
    glow, startCenter: CGPoint(x: tile.maxX - 150, y: tile.maxY - 120), startRadius: 0,
    endCenter: CGPoint(x: tile.maxX - 150, y: tile.maxY - 120), endRadius: 560, options: [])
ctx.restoreGState()

// Hairline edge, brighter at the top like a bevel.
ctx.saveGState()
ctx.addPath(tilePath)
ctx.setLineWidth(3)
ctx.replacePathWithStrokedPath()
ctx.clip()
let edge = CGGradient(
    colorsSpace: space, colors: [rgb(0xffffff, 0.16), rgb(0xffffff, 0.03)] as CFArray,
    locations: [0, 1])!
ctx.drawLinearGradient(
    edge, start: CGPoint(x: 0, y: tile.maxY), end: CGPoint(x: 0, y: tile.minY), options: [])
ctx.restoreGState()

func draw(_ text: String, font: String, size pointSize: CGFloat, color: CGColor, kern: CGFloat = 0,
          at origin: (CGRect) -> CGPoint) {
    let ctFont = CTFontCreateWithName(font as CFString, pointSize, nil)
    let attributes: [NSAttributedString.Key: Any] = [
        .init(kCTFontAttributeName as String): ctFont,
        .init(kCTForegroundColorAttributeName as String): color,
        .kern: kern,
    ]
    let line = CTLineCreateWithAttributedString(NSAttributedString(string: text, attributes: attributes))
    let bounds = CTLineGetBoundsWithOptions(line, .useGlyphPathBounds)
    ctx.textPosition = origin(bounds)
    CTLineDraw(line, ctx)
}

// "Xe", optically centred (glyph bounds, nudged down slightly).
draw("Xe", font: "Archivo-SemiBold", size: 440, color: rgb(0xe8eaee), kern: -12) { b in
    CGPoint(x: tile.midX - b.width / 2 - b.minX, y: tile.midY - b.height / 2 - b.minY - 18)
}

// Atomic number, top left.
draw("54", font: "IBMPlexMono-Medium", size: 76, color: rgb(0x8fa3bd)) { b in
    CGPoint(x: tile.minX + 92 - b.minX, y: tile.maxY - 92 - b.maxY)
}

// A single steel bar under the symbol: the only accent.
ctx.setFillColor(rgb(0x8fa3bd))
ctx.fill(CGRect(x: tile.midX - 70, y: tile.minY + 128, width: 140, height: 10))

let image = ctx.makeImage()!
let rep = NSBitmapImageRep(cgImage: image)
try! rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: output))
print("wrote \(output)")
