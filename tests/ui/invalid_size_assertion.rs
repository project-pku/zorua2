use zorua::prelude::*;

bitstruct! {
    #[repr(C)]
    #[size(bytes = 3)]
    struct Bad {
        pub a: u8,
        pub b: u8,
    }
}

fn main() {}
