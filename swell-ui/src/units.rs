use crate::Window;

/// An abstract unit used for dialog dimensions, independent of HiDPI and stuff.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
pub struct DialogUnits(pub u32);

impl DialogUnits {
    pub fn get(self) -> u32 {
        self.0
    }

    pub fn as_raw(self) -> i32 {
        self.0 as _
    }
}

/// Pixels on a screen.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
pub struct Pixels(pub u32);

impl Pixels {
    pub fn get(self) -> u32 {
        self.0
    }

    pub fn as_raw(self) -> i32 {
        self.0 as _
    }
}

/// Point in a coordinate system.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
pub struct Point<T> {
    pub x: T,
    pub y: T,
}

impl<T> Point<T> {
    pub const fn new(x: T, y: T) -> Self {
        Self { x, y }
    }
}

impl Point<DialogUnits> {
    /// A value used for calculating window size and spacing from dialog units.
    ///
    /// Might have to be chosen a bit differently on each OS.
    const UI_SCALE_FACTOR: f64 = 3.5;

    /// Converts this dialog unit point to pixels.
    ///
    /// The Window struct contains a method which can do this including Windows HiDPI information.
    pub fn in_pixels(&self) -> Point<Pixels> {
        // TODO On Windows this works differently. See original ReaLearn. But on the other hand
        //  ... this is only for the first short render before the optimal size is calculated.
        //  So as long as it works, this heuristic is okay.
        Point {
            x: Pixels((Self::UI_SCALE_FACTOR * self.x.get() as f64) as _),
            y: Pixels((Self::UI_SCALE_FACTOR * self.y.get() as f64) as _),
        }
    }
}

impl<T: Copy> Point<T> {
    pub fn to_dimensions(&self) -> Dimensions<T> {
        Dimensions::new(self.x, self.y)
    }
}

impl<T: Copy> From<Dimensions<T>> for Point<T> {
    fn from(d: Dimensions<T>) -> Self {
        d.to_point()
    }
}

/// Dimensions of a rectangle.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
pub struct Dimensions<T> {
    pub width: T,
    pub height: T,
}

impl<T> Dimensions<T> {
    pub const fn new(width: T, height: T) -> Self {
        Self { width, height }
    }
}

impl<T: Copy> Dimensions<T> {
    pub fn to_point(&self) -> Point<T> {
        Point::new(self.width, self.height)
    }
}

impl Dimensions<Pixels> {
    pub fn to_vst(&self) -> (i32, i32) {
        (self.width.get() as _, self.height.get() as _)
    }
}

impl Dimensions<DialogUnits> {
    /// Converts the given dialog unit dimensions to pixels.
    pub fn in_pixels(&self) -> Dimensions<Pixels> {
        self.to_point().in_pixels().to_dimensions()
    }
}

impl<T: Copy> From<Point<T>> for Dimensions<T> {
    fn from(p: Point<T>) -> Self {
        p.to_dimensions()
    }
}
