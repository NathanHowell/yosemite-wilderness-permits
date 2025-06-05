# yosemite_wilderness_permits

yosemite_wilderness_permits is a Rust command-line tool that scrapes the Yosemite "WildTrails" plugin API to report wilderness permit availability per trailhead and date.

## Features

- Fetch Yosemite trailhead metadata and daily occupancy reports
- Compute availability (capacity/quotas minus occupied spots, with 15-day walk-up window)
- Output CSV of `date,trailhead_name,availability`
- Built-in JSON parsing tests against sample fixtures

## Requirements

- Rust (Edition 2018 or later)
- A valid browser session `COOKIE` from yosemite.org

## Installation

```bash
git clone https://github.com/NathanHowell/yosemite.git
cd yosemite
cargo build --release
```

## Usage

You can provide the Yosemite session cookie via the `COOKIE` environment variable:

```bash
COOKIE="YOUR_SESSION_COOKIE" ./target/release/yosemite_wilderness_permits > availability.csv
```

If `COOKIE` is not set, the tool will prompt you to enter it interactively.

## Sample Output

See `foo.txt` for an example CSV output:

```text
2020-10-02,Alder Creek,30
2020-10-02,Beehive Meadow,28
...
```

## Testing

Parse the included JSON fixtures and run the lightweight unit tests:

```bash
cargo test
```

## License

This project is licensed under the MIT License. See [LICENSE](LICENSE) for details.