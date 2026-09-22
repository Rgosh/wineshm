//! Read a block somebody is publishing, on Linux.
//!
//! The whole of what integrating this looks like. Start the bridge however
//! you like — by hand, or with the `launch` example next door — and then:
//!
//! ```bash
//! cargo run --example read_a_block -- acpmf_physics:2048
//! ```

fn main() -> std::process::ExitCode {
    let Some(spec) = std::env::args().nth(1) else {
        eprintln!("usage: read_a_block NAME:SIZE   (try acpmf_physics:2048)");
        return std::process::ExitCode::from(2);
    };

    let page = match wineshm::Page::parse(&spec) {
        Ok(page) => page,
        Err(why) => {
            eprintln!("{spec}: {why}");
            return std::process::ExitCode::from(2);
        }
    };

    let store = wineshm::Store::default();

    // Worth asking first: it tells "nothing is publishing" apart from "that
    // block is not one of the ones being published", and the two need
    // different things done about them.
    match wineshm::reader::publishing(&store) {
        Some(note) => println!(
            "wineshm {} is publishing {} block(s)",
            note.version,
            note.pages.len()
        ),
        None => {
            eprintln!("nothing is publishing in {}", store.dir().display());
            eprintln!("start the bridge inside the prefix first");
            return std::process::ExitCode::FAILURE;
        }
    }

    let reader = match wineshm::reader::Reader::open(&store, &page) {
        Ok(reader) => reader,
        Err(why) => {
            eprintln!("could not open {}: {why}", page.name);
            return std::process::ExitCode::FAILURE;
        }
    };

    // One buffer, reused. Allocating sixty times a second to hold two
    // kilobytes is work nobody asked for.
    let mut bytes = vec![0u8; page.bytes];
    for tick in 0..10 {
        if let Err(why) = reader.read_into(&mut bytes) {
            eprintln!("read failed: {why}");
            return std::process::ExitCode::FAILURE;
        }
        let interesting = bytes.iter().filter(|byte| **byte != 0).count();
        println!(
            "{tick:>3}  first 8 bytes {:02x?}  {interesting} of {} bytes are not zero",
            &bytes[..8.min(bytes.len())],
            bytes.len()
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    std::process::ExitCode::SUCCESS
}
