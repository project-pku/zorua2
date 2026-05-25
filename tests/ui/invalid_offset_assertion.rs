use zorua::prelude::*;

bitstruct! {
    #[repr(C)]
    #[offset(b = 2)]
    struct Bad {
        pub a: u8,
        pub b: u8,
    }
}

fn main() {}
