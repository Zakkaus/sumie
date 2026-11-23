use super::Cell;

// One hex digit per arm: left, right, up, down. 0 = absent, 1 = light, 2 = heavy.
#[rustfmt::skip]
const JOINTS: [u16; 64] = [
    0x0101, 0x0201, 0x0102, 0x0202, 0x1001, 0x2001, 0x1002, 0x2002, // ┌ … ┓
    0x0110, 0x0210, 0x0120, 0x0220, 0x1010, 0x2010, 0x1020, 0x2020, // └ … ┛
    0x0111, 0x0211, 0x0121, 0x0112, 0x0122, 0x0221, 0x0212, 0x0222, // ├ … ┣
    0x1011, 0x2011, 0x1021, 0x1012, 0x1022, 0x2021, 0x2012, 0x2022, // ┤ … ┫
    0x1101, 0x2101, 0x1201, 0x2201, 0x1102, 0x2102, 0x1202, 0x2202, // ┬ … ┳
    0x1110, 0x2110, 0x1210, 0x2210, 0x1120, 0x2120, 0x1220, 0x2220, // ┴ … ┻
    0x1111, 0x2111, 0x1211, 0x2211, 0x1121, 0x1112, 0x1122, 0x2121, // ┼ … ╃
    0x1221, 0x2112, 0x1212, 0x2221, 0x2212, 0x2122, 0x1222, 0x2222, // ╄ … ╋
];

// At a double junction the center is a 3×3 tile. Each octal digit is one row
// of three bits (leftmost first); each arm continues lanes 010 or 101 to the edge.
#[rustfmt::skip]
const DOUBLE: [(u16, u16); 29] = [
    (0x5500, 0o707), (0x0055, 0o555), // ═ ║
    (0x0502, 0o323), (0x0205, 0o075), (0x0505, 0o745), // ╒ ╓ ╔
    (0x5002, 0o626), (0x2005, 0o075), (0x5005, 0o715), // ╕ ╖ ╗
    (0x0520, 0o323), (0x0250, 0o570), (0x0550, 0o547), // ╘ ╙ ╚
    (0x5020, 0o626), (0x2050, 0o570), (0x5050, 0o517), // ╛ ╜ ╝
    (0x0522, 0o323), (0x0255, 0o555), (0x0555, 0o545), // ╞ ╟ ╠
    (0x5022, 0o626), (0x2055, 0o555), (0x5055, 0o515), // ╡ ╢ ╣
    (0x5502, 0o707), (0x2205, 0o075), (0x5505, 0o705), // ╤ ╥ ╦
    (0x5520, 0o707), (0x2250, 0o570), (0x5550, 0o507), // ╧ ╨ ╩
    (0x5522, 0o727), (0x2255, 0o575), (0x5555, 0o505), // ╪ ╫ ╬
];

fn arms(code: u16) -> [u32; 4] {
    [12, 8, 4, 0].map(|shift| u32::from((code >> shift) & 15))
}

fn band(extent: u32, thickness: u32) -> (u32, u32) {
    let start = extent.saturating_sub(thickness) / 2;
    (start, (start + thickness).min(extent))
}

impl Cell<'_> {
    pub(super) fn box_lines(&mut self, cp: u32) {
        let code = match cp {
            0x2500..=0x2503 => [0x1100, 0x2200, 0x0011, 0x0022][(cp - 0x2500) as usize],
            0x250c..=0x254b => JOINTS[(cp - 0x250c) as usize],
            0x2550..=0x256c => return self.double(DOUBLE[(cp - 0x2550) as usize]),
            0x2574..=0x257f => [
                0x1000, 0x0010, 0x0100, 0x0001, 0x2000, 0x0020, 0x0200, 0x0002, 0x1200, 0x0012,
                0x2100, 0x0021,
            ][(cp - 0x2574) as usize],
            _ => return self.dashes(cp),
        };
        let [l, r, u, d] = arms(code).map(|weight| weight * self.light);
        let (x0, x1) = band(self.width, u.max(d));
        let (y0, y1) = band(self.height, l.max(r));
        for (weight, start, end) in [(l, 0, x1), (r, x0, self.width)] {
            let (top, bottom) = band(self.height, weight);
            self.rect(start, top, end, bottom, 255);
        }
        for (weight, start, end) in [(u, 0, y1), (d, y0, self.height)] {
            let (left, right) = band(self.width, weight);
            self.rect(left, start, right, end, 255);
        }
    }

    fn double(&mut self, (code, center): (u16, u16)) {
        let cuts = |size| {
            let (a, b) = band(size, self.light);
            [
                0,
                a.saturating_sub(self.light),
                a,
                b,
                (b + self.light).min(size),
                size,
            ]
        };
        let xs = cuts(self.width);
        let ys = cuts(self.height);
        let [left, right, up, down] = arms(code);
        for lane in 0..3 {
            let bit = 4 >> lane;
            let (i, j) = (lane + 1, lane + 2);
            if left & bit != 0 {
                self.rect(0, ys[i], xs[1], ys[j], 255);
            }
            if right & bit != 0 {
                self.rect(xs[4], ys[i], self.width, ys[j], 255);
            }
            if up & bit != 0 {
                self.rect(xs[i], 0, xs[j], ys[1], 255);
            }
            if down & bit != 0 {
                self.rect(xs[i], ys[4], xs[j], self.height, 255);
            }
            let row = center >> (6 - 3 * lane);
            for col in 0..3 {
                if row & (4 >> col) != 0 {
                    self.rect(xs[col + 1], ys[i], xs[col + 2], ys[j], 255);
                }
            }
        }
    }

    fn dashes(&mut self, cp: u32) {
        let count = match cp {
            0x2504..=0x2507 => 3,
            0x2508..=0x250b => 4,
            _ => 2,
        };
        let vertical = cp & 2 != 0;
        let (length, breadth) = if vertical {
            (self.height, self.width)
        } else {
            (self.width, self.height)
        };
        let (a, b) = band(breadth, (1 + (cp & 1)) * self.light);
        for i in 0..count {
            let start = length * i / count;
            let next = length * (i + 1) / count;
            let end = next - (next - start + 1) / 3;
            if vertical {
                self.rect(a, start, b, end, 255);
            } else {
                self.rect(start, a, end, b, 255);
            }
        }
    }
}
