use zorua::prelude::*;

bitstruct! {
    #[repr(C)]
    struct Bad {
        bits flags: u8 {
            enabled: bool,
        },
    }
}

fn main() {
    let bad = Bad::new_zeroed();
    let _ = bad.enabled();
}
