//! Git worktree management and name generator for Kobold sessions.
//!
//! Generates memorable, unique worktree names matching the pattern:
//! `{number}-{adjective}-{vehicle}-{2-random-bytes}` (e.g. `eleven-pink-trains-a1b2`, `one-big-car-4f2a`).
//!
//! Provides utilities to detect git repos, inspect worktrees, and isolate conflicting
//! concurrent daemon sessions into separate git worktrees.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Exact list of 20 English number words ("one" through "twenty").
pub const NUMBERS: [&str; 20] = [
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "ten",
    "eleven",
    "twelve",
    "thirteen",
    "fourteen",
    "fifteen",
    "sixteen",
    "seventeen",
    "eighteen",
    "nineteen",
    "twenty",
];

/// 256 distinct adjectives covering colors, sizes, moods, speeds, and states.
pub const ADJECTIVES: [&str; 256] = [
    // Colors (50)
    "amber",
    "aqua",
    "azure",
    "beige",
    "black",
    "blue",
    "bronze",
    "brown",
    "carmine",
    "cerulean",
    "charcoal",
    "cobalt",
    "copper",
    "coral",
    "crimson",
    "cyan",
    "emerald",
    "fawn",
    "gold",
    "golden",
    "gray",
    "green",
    "hazel",
    "indigo",
    "ivory",
    "jade",
    "khaki",
    "lavender",
    "lemon",
    "lilac",
    "lime",
    "magenta",
    "maroon",
    "mauve",
    "navy",
    "ochre",
    "olive",
    "orange",
    "peach",
    "pearl",
    "pink",
    "platinum",
    "plum",
    "purple",
    "red",
    "rose",
    "ruby",
    "rust",
    "saffron",
    "sapphire",
    // Sizes (36)
    "big",
    "broad",
    "bulky",
    "colossal",
    "compact",
    "deep",
    "dense",
    "epic",
    "giant",
    "grand",
    "great",
    "heavy",
    "huge",
    "immense",
    "jumbo",
    "lean",
    "light",
    "little",
    "macro",
    "mammoth",
    "massive",
    "micro",
    "mighty",
    "mini",
    "narrow",
    "petite",
    "plump",
    "shallow",
    "short",
    "slender",
    "slight",
    "slim",
    "small",
    "stout",
    "tall",
    "thick",
    // Speeds & Dynamics (24)
    "agile",
    "brisk",
    "dashing",
    "dynamic",
    "fast",
    "fleet",
    "flying",
    "gliding",
    "hyper",
    "nimble",
    "prompt",
    "quick",
    "rapid",
    "roaring",
    "rushing",
    "snappy",
    "sonic",
    "speedy",
    "steady",
    "swift",
    "turbo",
    "urgent",
    "zippy",
    "zoom",
    // Moods & Demeanor (70)
    "alert",
    "aloof",
    "amiable",
    "amused",
    "bold",
    "brave",
    "breezy",
    "bright",
    "buoyant",
    "calm",
    "candid",
    "carefree",
    "cheerful",
    "cheery",
    "clever",
    "cool",
    "crafty",
    "dapper",
    "daring",
    "defiant",
    "deft",
    "devoted",
    "eager",
    "earnest",
    "easy",
    "elated",
    "feisty",
    "fervent",
    "fiery",
    "frank",
    "friendly",
    "gallant",
    "gentle",
    "gleeful",
    "glowing",
    "graceful",
    "groovy",
    "happy",
    "hardy",
    "hearty",
    "heroic",
    "humble",
    "jaunty",
    "jolly",
    "jovial",
    "joyful",
    "keen",
    "kind",
    "lively",
    "loyal",
    "lucid",
    "mellow",
    "merry",
    "modest",
    "noble",
    "patient",
    "peppy",
    "perky",
    "plucky",
    "polite",
    "proud",
    "pure",
    "radiant",
    "regal",
    "robust",
    "serene",
    "sharp",
    "silly",
    "smart",
    "spry",
    // Elements, Textures & States (76)
    "ancient",
    "arcane",
    "astral",
    "atomic",
    "blunt",
    "bountiful",
    "bubbly",
    "chill",
    "clean",
    "clear",
    "cloudy",
    "cosmic",
    "crisp",
    "crystal",
    "cunning",
    "curious",
    "dazzling",
    "dewy",
    "dusty",
    "earthy",
    "fair",
    "fluffy",
    "foggy",
    "fresh",
    "frosty",
    "fuzzy",
    "glossy",
    "halcyon",
    "hard",
    "harsh",
    "hazy",
    "icy",
    "infinite",
    "iron",
    "keenly",
    "kinetic",
    "lunar",
    "magic",
    "misty",
    "modern",
    "muddy",
    "mystic",
    "neat",
    "nebular",
    "noblehearted",
    "novel",
    "odd",
    "polar",
    "prime",
    "pristine",
    "quaint",
    "quirky",
    "raw",
    "resilient",
    "rocky",
    "rough",
    "rustic",
    "sandy",
    "shiny",
    "silent",
    "slick",
    "smooth",
    "snowy",
    "solar",
    "solid",
    "sparkling",
    "starry",
    "stellar",
    "stormy",
    "sturdy",
    "subtle",
    "sunny",
    "tender",
    "tidy",
    "tough",
    "vibrant",
];

/// 256 distinct vehicles and transport machines covering land, rail, water, air, space, and sci-fi.
pub const VEHICLES: [&str; 256] = [
    // Land & Road (60)
    "car",
    "truck",
    "bus",
    "van",
    "cab",
    "taxi",
    "wagon",
    "cart",
    "buggy",
    "jeep",
    "suv",
    "sedan",
    "coupe",
    "pickup",
    "trailer",
    "tractor",
    "forklift",
    "bulldozer",
    "excavator",
    "steamroller",
    "grader",
    "scraper",
    "crane",
    "fireengine",
    "ambulance",
    "policecar",
    "limousine",
    "hearse",
    "camper",
    "motorhome",
    "moped",
    "scooter",
    "motorcycle",
    "motorbike",
    "chopper",
    "cruiser",
    "tricycle",
    "bicycle",
    "unicycle",
    "skateboard",
    "rollerblade",
    "hoverboard",
    "segway",
    "snowmobile",
    "sled",
    "sleigh",
    "carriage",
    "chariot",
    "rickshaw",
    "tramcar",
    "roadster",
    "dragster",
    "kart",
    "go-kart",
    "minivan",
    "hotrod",
    "flatbed",
    "tanker",
    "dumptruck",
    "snowplow",
    // Rail & Track (30)
    "train",
    "locomotive",
    "freightcar",
    "boxcar",
    "caboose",
    "railcoach",
    "trolley",
    "streetcar",
    "monorail",
    "subway",
    "metro",
    "bullettrain",
    "maglev",
    "handcar",
    "funicular",
    "cogwheel",
    "draisine",
    "cablecar",
    "incline",
    "tramway",
    "railcar",
    "lightrail",
    "switchcar",
    "streamliner",
    "autorail",
    "rackrail",
    "hopper",
    "tankwagon",
    "intercity",
    "express",
    // Water & Marine (66)
    "boat",
    "ship",
    "vessel",
    "yacht",
    "speedboat",
    "sailboat",
    "skiff",
    "dinghy",
    "canoe",
    "kayak",
    "raft",
    "catamaran",
    "trimaran",
    "rowboat",
    "paddleboat",
    "motorboat",
    "powerboat",
    "hydrofoil",
    "hovercraft",
    "tugboat",
    "barge",
    "ferry",
    "freighter",
    "cargoship",
    "container",
    "dreadnought",
    "galleon",
    "frigate",
    "corvette",
    "destroyer",
    "battleship",
    "submarine",
    "sub",
    "bathyscaphe",
    "runabout",
    "airboat",
    "trawler",
    "schooner",
    "sloop",
    "ketch",
    "brig",
    "clipper",
    "bark",
    "junk",
    "sampan",
    "dory",
    "punt",
    "gondola",
    "yawl",
    "ironclad",
    "longship",
    "warcanoe",
    "drakkar",
    "trireme",
    "bireme",
    "outrigger",
    "coracle",
    "currach",
    "packetboat",
    "wherry",
    "lugger",
    "smack",
    "drifter",
    "pilotboat",
    "lifeboat",
    "icebreaker",
    // Air & Flight (45)
    "plane",
    "airplane",
    "aeroplane",
    "jet",
    "biplane",
    "triplane",
    "glider",
    "sailplane",
    "ultralight",
    "hangglider",
    "paraglider",
    "helicopter",
    "gyrocopter",
    "autogyro",
    "tiltrotor",
    "airship",
    "blimp",
    "zeppelin",
    "balloon",
    "skyship",
    "jetpack",
    "dropship",
    "ornithopter",
    "flyer",
    "skyvan",
    "bijet",
    "trijet",
    "quadjet",
    "jumbojet",
    "seaplane",
    "floatplane",
    "flyingboat",
    "propeller",
    "turboprop",
    "airbus",
    "skycraft",
    "monoplane",
    "warplane",
    "bomber",
    "fighter",
    "intercept",
    "recon",
    "aerostat",
    "stratojet",
    "stealthjet",
    // Space & Sci-Fi (55)
    "rocket",
    "starship",
    "spaceship",
    "shuttle",
    "orbiter",
    "lander",
    "rover",
    "probe",
    "satellite",
    "capsule",
    "spacestation",
    "spacecraft",
    "starcruiser",
    "starfighter",
    "mothership",
    "freightersub",
    "transporter",
    "skimmer",
    "walker",
    "mecha",
    "battlewagon",
    "speeder",
    "swooper",
    "landspeeder",
    "snowspeeder",
    "skyhopper",
    "cloudcar",
    "escapepod",
    "speederbike",
    "battlecruiser",
    "starfreighter",
    "battlestar",
    "sailbarge",
    "sandcrawler",
    "warwagon",
    "dreadstar",
    "interceptor",
    "gunship",
    "corvair",
    "spacetug",
    "solarsail",
    "chronoship",
    "deepspace",
    "starliner",
    "warpship",
    "suborbital",
    "hyperdrive",
    "lightship",
    "voidcraft",
    "astrosloop",
    "starhauler",
    "stardory",
    "voidskiff",
    "planetlander",
    "hypercraft",
];

/// Pluralizes a noun according to standard English rules.
pub fn pluralize(word: &str) -> String {
    if word.ends_with("express") || word.ends_with("bus") {
        return format!("{word}es");
    }
    if word.ends_with('s')
        || word.ends_with("sh")
        || word.ends_with("ch")
        || word.ends_with('x')
        || word.ends_with('z')
    {
        format!("{word}es")
    } else if word.ends_with('y')
        && !word.ends_with("ay")
        && !word.ends_with("ey")
        && !word.ends_with("oy")
    {
        format!("{}ies", &word[..word.len() - 1])
    } else {
        format!("{word}s")
    }
}

/// Generates a random worktree name according to `{number}-{adjective}-{vehicle}-{2-random-bytes}`.
///
/// Uses singular vehicle for "one" (e.g. `one-big-car-4f2a`) and plural for numbers
/// greater than one (e.g. `eleven-pink-trains-b1c2`).
pub fn generate_worktree_name() -> String {
    let num_idx = fastrand::usize(0..NUMBERS.len());
    let adj_idx = fastrand::usize(0..ADJECTIVES.len());
    let veh_idx = fastrand::usize(0..VEHICLES.len());
    let bytes = [fastrand::u8(..), fastrand::u8(..)];

    generate_name_from_indices(num_idx, adj_idx, veh_idx, bytes)
}

/// Deterministically generates a worktree name from specific indices and 2 random bytes.
pub fn generate_name_from_indices(
    num_idx: usize,
    adj_idx: usize,
    veh_idx: usize,
    bytes: [u8; 2],
) -> String {
    let number = NUMBERS[num_idx % NUMBERS.len()];
    let adjective = ADJECTIVES[adj_idx % ADJECTIVES.len()];
    let base_vehicle = VEHICLES[veh_idx % VEHICLES.len()];

    let vehicle = if number == "one" {
        base_vehicle.to_string()
    } else {
        pluralize(base_vehicle)
    };

    let hex_suffix = format!("{:02x}{:02x}", bytes[0], bytes[1]);
    format!("{number}-{adjective}-{vehicle}-{hex_suffix}")
}

/// Parsed elements of a worktree name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeNameParts {
    pub number: String,
    pub adjective: String,
    pub vehicle: String,
    pub random_bytes_hex: String,
}

/// Parses a worktree name back into its 4 component parts.
pub fn parse_worktree_name(name: &str) -> Option<WorktreeNameParts> {
    let parts: Vec<&str> = name.split('-').collect();
    if parts.len() < 4 {
        return None;
    }

    let number = parts[0];
    if !NUMBERS.contains(&number) {
        return None;
    }

    let adjective = parts[1];
    if !ADJECTIVES.contains(&adjective) {
        return None;
    }

    let random_bytes_hex = parts[parts.len() - 1];
    if random_bytes_hex.len() != 4 || !random_bytes_hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }

    // The vehicle may contain hyphens (e.g. go-kart)
    let vehicle = parts[2..parts.len() - 1].join("-");

    Some(WorktreeNameParts {
        number: number.to_string(),
        adjective: adjective.to_string(),
        vehicle,
        random_bytes_hex: random_bytes_hex.to_string(),
    })
}

/// Checks if the given path is inside a Git repository.
pub fn is_git_repo(path: &Path) -> bool {
    let status = Command::new("git")
        .arg("rev-parse")
        .arg("--is-inside-work-tree")
        .current_dir(path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    matches!(status, Ok(s) if s.success())
}

/// Finds the root directory of the Git repository enclosing `path`.
pub fn find_git_root(path: &Path) -> Option<PathBuf> {
    let output = Command::new("git")
        .arg("rev-parse")
        .arg("--show-toplevel")
        .current_dir(path)
        .output()
        .ok()?;

    if output.status.success() {
        let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !s.is_empty() {
            return Some(PathBuf::from(s));
        }
    }
    None
}

/// Creates a new Git worktree under `.worktrees/<name>` branching off the current HEAD.
///
/// Ensures `.worktrees/` is added to `.git/info/exclude` so working directory status
/// of the main repository remains unaffected.
pub fn create_git_worktree(repo_root: &Path, name: &str) -> io::Result<PathBuf> {
    let worktrees_dir = repo_root.join(".worktrees");
    std::fs::create_dir_all(&worktrees_dir)?;

    // Ensure .worktrees is excluded in .git/info/exclude if info directory exists
    let exclude_file = repo_root.join(".git").join("info").join("exclude");
    if exclude_file.exists() {
        if let Ok(content) = std::fs::read_to_string(&exclude_file) {
            if !content
                .lines()
                .any(|l| l.trim() == ".worktrees" || l.trim() == ".worktrees/")
            {
                use std::io::Write as _;
                if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(&exclude_file) {
                    let _ = writeln!(f, ".worktrees");
                }
            }
        }
    }

    let worktree_path = worktrees_dir.join(name);

    let output = Command::new("git")
        .arg("worktree")
        .arg("add")
        .arg(&worktree_path)
        .arg("-b")
        .arg(name)
        .current_dir(repo_root)
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(io::Error::other(format!(
            "failed to create git worktree '{name}': {stderr}"
        )));
    }

    Ok(worktree_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_list_has_exact_length_20() {
        assert_eq!(NUMBERS.len(), 20);
        assert_eq!(NUMBERS[0], "one");
        assert_eq!(NUMBERS[19], "twenty");
    }

    #[test]
    fn adjectives_list_has_exact_length_256_and_all_unique() {
        assert_eq!(ADJECTIVES.len(), 256);
        let mut set = std::collections::HashSet::new();
        for &adj in &ADJECTIVES {
            assert!(set.insert(adj), "duplicate adjective found: {adj}");
        }
    }

    #[test]
    fn vehicles_list_has_exact_length_256_and_all_unique() {
        assert_eq!(VEHICLES.len(), 256);
        let mut set = std::collections::HashSet::new();
        for &v in &VEHICLES {
            assert!(set.insert(v), "duplicate vehicle found: {v}");
        }
    }

    #[test]
    fn pluralization_rules() {
        assert_eq!(pluralize("car"), "cars");
        assert_eq!(pluralize("train"), "trains");
        assert_eq!(pluralize("bus"), "buses");
        assert_eq!(pluralize("ferry"), "ferries");
        assert_eq!(pluralize("express"), "expresses");
    }

    #[test]
    fn name_generation_format_and_roundtrip() {
        let name1 = generate_name_from_indices(0, 0, 0, [0x4f, 0x2a]);
        // index 0 is "one", adjective 0 is "amber", vehicle 0 is "car"
        assert_eq!(name1, "one-amber-car-4f2a");

        let parsed1 = parse_worktree_name(&name1).expect("parse one-amber-car");
        assert_eq!(parsed1.number, "one");
        assert_eq!(parsed1.adjective, "amber");
        assert_eq!(parsed1.vehicle, "car");
        assert_eq!(parsed1.random_bytes_hex, "4f2a");

        let name2 = generate_name_from_indices(10, 40, 60, [0xa1, 0xb2]);
        // index 10 is "eleven", adjective 40 is "pink", vehicle 60 is "train"
        assert_eq!(name2, "eleven-pink-trains-a1b2");

        let parsed2 = parse_worktree_name(&name2).expect("parse eleven-pink-trains");
        assert_eq!(parsed2.number, "eleven");
        assert_eq!(parsed2.adjective, "pink");
        assert_eq!(parsed2.vehicle, "trains");
        assert_eq!(parsed2.random_bytes_hex, "a1b2");
    }

    #[test]
    fn random_name_is_valid() {
        for _ in 0..100 {
            let name = generate_worktree_name();
            let parsed = parse_worktree_name(&name);
            assert!(parsed.is_some(), "generated name invalid: {name}");
        }
    }
}
