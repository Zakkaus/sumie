import CoreGraphics
import Foundation
import ImageIO

guard CommandLine.arguments.count == 2,
      let source = CGImageSourceCreateWithURL(URL(fileURLWithPath: CommandLine.arguments[1]) as CFURL, nil),
      let image = CGImageSourceCreateImageAtIndex(source, 0, nil),
      image.width >= 300, image.height >= 120 else {
    exit(2)
}

// The smoke page has no glyphs at this location. Convert the patch from its embedded profile to sRGB.
let side = 5
let rowBytes = side * 4
let rect = CGRect(x: image.width * 3 / 4 - 2, y: image.height / 2 - 2, width: side, height: side)
let format = CGBitmapInfo.byteOrder32Big.rawValue | CGImageAlphaInfo.premultipliedLast.rawValue
guard let patch = image.cropping(to: rect),
      let colorSpace = CGColorSpace(name: CGColorSpace.sRGB),
      let context = CGContext(data: nil, width: side, height: side, bitsPerComponent: 8,
                              bytesPerRow: rowBytes, space: colorSpace, bitmapInfo: format),
      let data = context.data else {
    exit(2)
}
context.draw(patch, in: CGRect(x: 0, y: 0, width: side, height: side))
let rgba = data.assumingMemoryBound(to: UInt8.self)
for index in stride(from: 0, to: side * rowBytes, by: 4) {
    guard rgba[index] >= 230, rgba[index + 1] <= 32, rgba[index + 2] >= 230,
          rgba[index + 3] == 255 else {
        exit(1)
    }
}
