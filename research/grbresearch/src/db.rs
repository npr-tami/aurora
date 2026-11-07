//! research.json: everything learned about the game, with how sure we are. Hand-editable. Status
//! rules: verified = a controlled test proved it; strong = a working, live-used feature depends on
//! it; unverified = a correlate, guess or parked lead; rejected = ruled out (with the evidence).
//! The seed never says "verified".

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

pub const SECTIONS: [&str; 9] = [
    "profiles",
    "vtables",
    "object_types",
    "offsets",
    "relationships",
    "functions",
    "signatures",
    "experiments",
    "notes",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Verified,
    Strong,
    #[default]
    Unverified,
    Rejected,
}

impl Status {
    pub fn parse(s: &str) -> Result<Status, String> {
        Ok(match s.to_ascii_lowercase().as_str() {
            "verified" => Status::Verified,
            "strong" => Status::Strong,
            "unverified" => Status::Unverified,
            "rejected" => Status::Rejected,
            _ => return Err(format!("unknown status '{s}' (verified | strong | unverified | rejected)")),
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Status::Verified => "verified",
            Status::Strong => "strong",
            Status::Unverified => "unverified",
            Status::Rejected => "rejected",
        }
    }
}

/// One fact. Empty fields stay in the file so it is clear what can be filled in.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Entry {
    pub id: String,
    pub name: String,
    pub status: Status,
    pub exe: String,
    pub build: String,
    /// Static address, e.g. "exe+0x49BFD70".
    pub module_rva: String,
    /// Offset inside an object, e.g. "0x20" (or a short chain "+0x60 -> +0x160 -> +0x104").
    pub object_offset: String,
    pub object_type: String,
    pub data_type: String,
    pub how_found: String,
    pub how_tested: String,
    pub evidence: String,
    pub limitations: String,
    pub date: String,
    /// Any other keys (pattern, value, source...).
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Db {
    pub about: String,
    pub profiles: Vec<Entry>,
    pub vtables: Vec<Entry>,
    pub object_types: Vec<Entry>,
    pub offsets: Vec<Entry>,
    pub relationships: Vec<Entry>,
    pub functions: Vec<Entry>,
    pub signatures: Vec<Entry>,
    pub experiments: Vec<Entry>,
    pub notes: Vec<Entry>,
}

impl Db {
    pub fn section(&self, name: &str) -> Option<&Vec<Entry>> {
        Some(match name {
            "profiles" => &self.profiles,
            "vtables" => &self.vtables,
            "object_types" => &self.object_types,
            "offsets" => &self.offsets,
            "relationships" => &self.relationships,
            "functions" => &self.functions,
            "signatures" => &self.signatures,
            "experiments" => &self.experiments,
            "notes" => &self.notes,
            _ => return None,
        })
    }

    pub fn section_mut(&mut self, name: &str) -> Option<&mut Vec<Entry>> {
        Some(match name {
            "profiles" => &mut self.profiles,
            "vtables" => &mut self.vtables,
            "object_types" => &mut self.object_types,
            "offsets" => &mut self.offsets,
            "relationships" => &mut self.relationships,
            "functions" => &mut self.functions,
            "signatures" => &mut self.signatures,
            "experiments" => &mut self.experiments,
            "notes" => &mut self.notes,
            _ => return None,
        })
    }

    /// Every (section, entry).
    pub fn all(&self) -> Vec<(&'static str, &Entry)> {
        SECTIONS
            .iter()
            .flat_map(|s| self.section(s).into_iter().flatten().map(move |e| (*s, e)))
            .collect()
    }

    pub fn find_id_mut(&mut self, id: &str) -> Option<&mut Entry> {
        let sec = SECTIONS
            .iter()
            .find(|s| self.section(s).is_some_and(|v| v.iter().any(|e| e.id.eq_ignore_ascii_case(id))))?;
        self.section_mut(sec)?.iter_mut().find(|e| e.id.eq_ignore_ascii_case(id))
    }

    /// Entries where any field contains `text` (case-insensitive).
    pub fn find(&self, text: &str) -> Vec<(&'static str, &Entry)> {
        let needle = text.to_ascii_lowercase();
        self.all()
            .into_iter()
            .filter(|(s, e)| {
                s.contains(&needle)
                    || serde_json::to_string(e).is_ok_and(|j| j.to_ascii_lowercase().contains(&needle))
            })
            .collect()
    }

    pub fn count_by_status(&self) -> BTreeMap<Status, usize> {
        let mut m = BTreeMap::new();
        for (_, e) in self.all() {
            *m.entry(e.status).or_insert(0) += 1;
        }
        m
    }

    pub fn load(path: &Path) -> Result<Db, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Load, or seed and save it if the file does not exist yet.
    pub fn open(path: &Path, today: &str) -> Result<Db, String> {
        if path.exists() {
            return Db::load(path);
        }
        let db = seed(today);
        db.save(path)?;
        Ok(db)
    }
}

/// One line: "[strong] off.body_pos  Body +0x20  f64x3  player body position".
pub fn line(section: &str, e: &Entry) -> String {
    let mut s = format!("[{}] {} ({section})", e.status.name(), e.id);
    let place = match (e.module_rva.is_empty(), e.object_offset.is_empty()) {
        (false, _) => e.module_rva.clone(),
        (true, false) => format!("{} {}", if e.object_type.is_empty() { "?" } else { &e.object_type }, e.object_offset),
        _ => String::new(),
    };
    for part in [place.as_str(), e.data_type.as_str(), e.name.as_str()] {
        if !part.is_empty() {
            s.push_str("  ");
            s.push_str(part);
        }
    }
    s
}

/// The fields worth reading below a line.
pub fn details(e: &Entry) -> Vec<String> {
    let mut out = Vec::new();
    for (k, v) in [
        ("exe", &e.exe),
        ("build", &e.build),
        ("found", &e.how_found),
        ("tested", &e.how_tested),
        ("evidence", &e.evidence),
        ("limits", &e.limitations),
        ("date", &e.date),
    ] {
        if !v.is_empty() {
            out.push(format!("    {k}: {v}"));
        }
    }
    for (k, v) in &e.extra {
        out.push(format!("    {k}: {}", v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())));
    }
    out
}

/// Fill a field by key from `db add`; unknown keys go to `extra`.
pub fn set_field(e: &mut Entry, key: &str, value: &str) -> Result<(), String> {
    let v = value.to_string();
    match key {
        "id" => e.id = v,
        "name" => e.name = v,
        "status" => e.status = Status::parse(value)?,
        "exe" => e.exe = v,
        "build" => e.build = v,
        "module_rva" | "rva" => e.module_rva = v,
        "object_offset" | "offset" => e.object_offset = v,
        "object_type" | "type" => e.object_type = v,
        "data_type" => e.data_type = v,
        "how_found" | "found" => e.how_found = v,
        "how_tested" | "tested" => e.how_tested = v,
        "evidence" => e.evidence = v,
        "limitations" | "limits" => e.limitations = v,
        "date" => e.date = v,
        _ => {
            e.extra.insert(key.to_string(), serde_json::Value::String(v));
        }
    }
    Ok(())
}

/// Parse "0x20" / "20" (hex) from an object_offset field; chains give None.
pub fn offset_value(s: &str) -> Option<u64> {
    let t = s.trim().trim_start_matches('+');
    let t = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")).unwrap_or(t);
    u64::from_str_radix(t, 16).ok()
}

struct B(Entry);

impl B {
    fn rva(mut self, s: &str) -> B {
        self.0.module_rva = s.into();
        self
    }
    fn off(mut self, ty: &str, off: &str) -> B {
        self.0.object_type = ty.into();
        self.0.object_offset = off.into();
        self
    }
    fn ty(mut self, ty: &str) -> B {
        self.0.object_type = ty.into();
        self
    }
    fn dt(mut self, s: &str) -> B {
        self.0.data_type = s.into();
        self
    }
    fn found(mut self, s: &str) -> B {
        self.0.how_found = s.into();
        self
    }
    fn tested(mut self, s: &str) -> B {
        self.0.how_tested = s.into();
        self
    }
    fn ev(mut self, s: &str) -> B {
        self.0.evidence = s.into();
        self
    }
    fn lim(mut self, s: &str) -> B {
        self.0.limitations = s.into();
        self
    }
    fn x(mut self, k: &str, v: &str) -> B {
        self.0.extra.insert(k.into(), serde_json::Value::String(v.into()));
        self
    }
}

const VK: &str = "GRB_vulkan.exe";
const LIVE: &str = "Used every session by a working mod feature (live use, not a controlled test).";
const SRC: &str = "Taken from the mod source.";

fn seed(today: &str) -> Db {
    use Status::{Rejected as R, Strong as S, Unverified as U};
    let mk = |id: &str, name: &str, status: Status| {
        B(Entry {
            id: id.into(),
            name: name.into(),
            status,
            exe: VK.into(),
            build: "unknown".into(),
            date: today.into(),
            ..Default::default()
        })
    };
    let strong = |id: &str, name: &str, feature: &str| {
        mk(id, name, S).found(SRC).ev(&format!("{LIVE} Feature: {feature}."))
    };
    let mut db = Db {
        about: "Ghost Recon Breakpoint research notes. Status: verified = controlled test proved it; strong = a working \
                feature uses it live; unverified = correlate, guess or parked; rejected = ruled out. Edit by hand or \
                with `grbresearch db`."
            .into(),
        ..Default::default()
    };

    db.profiles = vec![
        mk("prof.grb_vulkan", "Vulkan build (the one the mod runs on)", S)
            .found(SRC)
            .ev("Every strong entry here is used live by the mod on GRB_vulkan.exe.")
            .lim("Build not recorded in the inventory: run `grbresearch info` and set build here.")
            .0,
        Entry {
            exe: "GRB.exe".into(),
            build: "unknown".into(),
            ..mk("prof.grb_dx11", "DX11 build (placeholder)", U)
                .lim("Nothing here was tested on GRB.exe. Its addresses must not be assumed equal to GRB_vulkan.exe.")
                .0
        },
    ];

    db.vtables = vec![
        strong("vt.char_component", "NPC character component (marker)", "ESP, Target")
            .rva("exe+0x49BFD70")
            .ty("CharComponent")
            .x("source", "entities.rs:133-143")
            .0,
        strong("vt.player_component", "player character component (marker)", "god mode entity, weapon checks")
            .rva("exe+0x4A4C5F0")
            .ty("PlayerComponent")
            .x("source", "entities.rs:133-143")
            .0,
    ];

    db.object_types = vec![
        strong("type.body", "Body: first qword is a class tag pointing into the exe (read it live)", "Fly, Teleport, ESP")
            .ty("Body")
            .lim("The class tag is not a fixed constant in code.")
            .x("source", "entities.rs:46, fly.rs:13-15")
            .0,
        strong("type.camera", "Camera / player root object", "ESP projection")
            .rva("[exe+0x62F24B0]")
            .ty("Camera")
            .x("source", "vkdraw.rs:34-45")
            .0,
        strong("type.pose_object", "Full-rig pose object (r8 of POSE_FN)", "ESP > Bones")
            .ty("PoseObject")
            .x("layout", "world pos 3 x f64 at +0x0, list ptr +0x248, 0x20 per entry, 431-456 entries")
            .x("source", "vkdraw.rs:1619-1637")
            .0,
        mk("type.awareness", "per-NPC awareness object (rcx of the awareness tick)", U)
            .ty("Awareness")
            .found("awarehook.rs recorder (research)")
            .lim("Priority 3 lead; layout partly known.")
            .0,
    ];

    db.offsets = vec![
        strong("off.body_root", "player body pointer", "Fly, Teleport, ESP, god mode")
            .rva("exe+0x63EBEB0")
            .dt("ptr -> Body")
            .x("check", "body >= 0x10000 and 24 bytes readable at body+0x20")
            .0,
        strong("off.body_pos", "position x y z", "Fly, Teleport, ESP").off("Body", "0x20").dt("f64 x3").0,
        strong("off.body_tag", "class tag (pointer into exe)", "ESP validity, move_body guard").off("Body", "0x0").dt("ptr").0,
        strong("off.body_flags", "flags, bit 0x4000 = dead", "ESP").off("Body", "0x108").dt("u32").0,
        strong("off.body_facing", "facing: 3 rows x 4 f32, rotation about up", "ESP bone placement")
            .off("Body", "0x40")
            .dt("f32 x12")
            .0,
        strong("off.marker_body", "component -> its body", "ESP, Target, god mode")
            .off("CharComponent", "0x8")
            .dt("ptr -> Body")
            .x("also", "PlayerComponent+0x8 is the player's body")
            .0,
        strong("off.side", "side: 3 = enemy, 0 = friendly, valid 0..64", "ESP colours")
            .off("CharComponent", "+0x60 -> +0x160 -> +0x104")
            .dt("i32")
            .0,
        strong("off.bone_obj", "bone object: two levels under owner, obj+0x10 == body, obj+0x308 -> pose list", "ESP > Bones")
            .ty("CharComponent")
            .dt("ptr")
            .x("pose_list", "18 bones x 0x30 (pos xyzw f32, quat, scale), body space")
            .0,
        mk("off.bone_obj_shortcut", "bone object often at owner+0x60 -> +0x230", U)
            .off("CharComponent", "+0x60 -> +0x230")
            .found(SRC)
            .lim("Not always true (find_bones searches instead).")
            .0,
        strong("off.cam_pos", "camera position", "ESP").off("Camera", "0x1C0").dt("f64 x3").0,
        strong("off.cam_right", "camera right", "ESP").off("Camera", "0x1E0").dt("f32 x3").0,
        strong("off.cam_forward", "camera forward", "ESP, Fly direction").off("Camera", "0x1F0").dt("f32 x3").0,
        strong("off.cam_up", "camera up", "ESP").off("Camera", "0x200").dt("f32 x3").0,
        mk("off.cam_fov", "vertical FOV in radians (1.13 = 65 deg)", U)
            .off("Camera", "0x1B0")
            .dt("f32")
            .found(SRC)
            .lim("The source calls it a guess that fits.")
            .0,
        mk("off.move_state", "ground/air state word: 0x00010001 on ground, 0 in air", R)
            .off("Body", "+0x90 -> +0x180 -> +0x474")
            .dt("u32")
            .found("fly.rs stand modes (parked 1 Nov)")
            .tested("Access watch while flying (standsearch, 1 Nov).")
            .ev("Never read while flying, so it is a correlate, not a control. Rejected as a control.")
            .0,
        mk("off.move_state_twins", "+0x554, +0x578, +0x584 flip with +0x474", U)
            .off("Body", "+0x90 -> +0x180 -> +0x554/+0x578/+0x584")
            .found(SRC)
            .lim("Correlates of the ground/air state; not tested as controls.")
            .0,
        mk("off.body_278", "[body+0x278]", R)
            .off("Body", "0x278")
            .dt("ptr")
            .found("standsearch.rs:35")
            .ev("Not stable (standsearch.rs:35).")
            .0,
        mk("off.mode_air_flag", "air flag byte +0x2C2 (flags +0x2C0..+0x2C4), mode at +0x2B8 (5 ground, 1 air)", U)
            .off("MovementMode", "0x2C2")
            .found("Mode pick function exe+0x2A5A4C0 (fly.rs:57-68)")
            .lim("Parked stand experiment; never active in the player's use.")
            .0,
        strong("off.immune", "god mode immune byte (0/1)", "God mode").off("Entity", "0x378").dt("u8").0,
        strong("off.net_part", "character network part", "Weapon").off("PlayerComponent", "0x190").dt("ptr").0,
        strong("off.stamina", "stamina byte [rdi+0x1F1], rdi+8 = character", "Weapon / stamina").off("Stamina", "0x1F1").dt("u8").0,
        strong("off.ammo_owner", "gun -> owner network part [rcx+0x88]", "Weapon / ammo").off("Gun", "0x88").dt("ptr").0,
        strong("off.credits_balance", "credits balance, XOR-scrambled (read only)", "Credits")
            .off("Credits", "0x20")
            .x("source", "credits.rs:20-36")
            .0,
        strong("off.gear_score", "gear score for NEW gear (read only)", "launcher gear hold")
            .rva("[exe+0x63A0D20]+0x490")
            .dt("i32")
            .lim("New gear only, not owned items.")
            .0,
        strong("off.sight_range", "sight-range factor load movss xmm15,[rcx+0x3C]", "Undetected")
            .off("Perception", "0x3C")
            .dt("f32")
            .0,
        mk("off.aware_meter", "awareness meter 0..100 (+0x228 twin, compared with +0x250)", U)
            .off("Awareness", "0x22C")
            .dt("f32")
            .found("awarehook / bonesprobe recorder (research)")
            .lim("Priority 3 lead. Other recorded fields: +0x234 +0x238 +0x224, flags +0x270..+0x274.")
            .0,
    ];

    db.relationships = vec![
        strong("rel.marker_body", "marker+0 == vtable and marker+8 == body; checked every frame", "ESP")
            .ty("CharComponent")
            .lim("Freed objects otherwise give ghost boxes.")
            .0,
        strong("rel.her_character", "the player's character = the PlayerComponent whose +8 is the player's body", "God mode, weapon")
            .ty("PlayerComponent")
            .0,
        strong("rel.weapon_character", "recoil/shake/sway: character at [rcx+8]", "Weapon").0,
    ];

    db.functions = vec![
        strong("fn.aware_fill", "awareness fill (FILL, CLAMP)", "Undetected").rva("exe+0x3634460").0,
        strong("fn.aware_twin", "awareness twin (SECOND)", "Undetected").rva("exe+0x363A6E0").0,
        mk("fn.aware_tick", "awareness tick, rcx = per-NPC awareness object", U)
            .rva("exe+0x35A0FD0")
            .found("awarehook.rs (research recorder)")
            .0,
        mk("fn.mode_pick", "movement mode pick", U)
            .rva("exe+0x2A5A4C0")
            .found("fly.rs stand modes")
            .lim("Parked 1 Nov.")
            .0,
        mk("fn.air_dispatch", "event dispatcher; id at +6, value at +7; id 0x49 value 1 = in the air", U)
            .rva("exe+0x1A21040")
            .found("standfilter.rs (stand C, parked)")
            .lim("Never active in the player's use.")
            .0,
        strong("fn.credits_read", "credits read (object in rcx), own-credits caller exe+0x273990E", "Credits")
            .rva("exe+0x1A72150")
            .0,
    ];

    let sig = |id: &str, name: &str, pat: &str, status: Status, why: &str| {
        let b = mk(id, name, status).found(SRC).x("pattern", pat);
        if status == S { b.ev(&format!("{LIVE} Feature: {why}.")) } else { b.lim(why) }
    };
    db.signatures = vec![
        sig(
            "sig.pose_fn",
            "POSE_FN, r8 = pose object",
            "48 89 5C 24 08 57 48 83 EC 30 49 8B 80 40 02 00 00 49 8B B8 48 02 00 00 4D 8B 81 48 02 00 00",
            S,
            "ESP > Bones",
        )
        .0,
        sig(
            "sig.godmode_damage",
            "damage check, entity in rdi",
            "48 8B ?? 48 8B ?? FF 90 ?? ?? ?? ?? 84 C0 0F 85 ?? ?? ?? ?? 44 38 A7",
            S,
            "God mode",
        )
        .0,
        sig("sig.credits_read", "credits read (function starts 5 bytes earlier)", "48 8B 01 33 D2 4C 8B C1", S, "Credits").0,
        B(sig("sig.sight_range", "sight-range factor load", "F3 44 0F 10 79 3C", S, "Undetected").0)
            .lim("Inventory gives only the start; the full pattern is in unseen.rs:26.")
            .0,
        sig(
            "sig.mode_pick",
            "movement mode pick",
            "80 BB C2 02 00 00 00 74 0C C7 83 B8 02 00 00 01 00 00 00",
            U,
            "Parked stand experiment.",
        )
        .0,
        sig(
            "sig.air_dispatch",
            "event dispatcher",
            "48 8B 06 44 0F B6 70 06 44 8B 38 44 0F B6 60 07 43 8D 0C 76 C1 E1 02 49 03 4D 40",
            U,
            "Parked stand C experiment.",
        )
        .0,
        sig(
            "sig.aware_tick",
            "awareness tick",
            "40 53 48 83 EC 60 48 8B D9 0F 29 7C 24 40 48 8B 49 28 0F 28 F9 48 85 C9",
            U,
            "Research recorder only.",
        )
        .0,
        sig(
            "sig.spot_damage",
            "who-runs spot: damage (weapon DAMAGE came from it)",
            "83 BF A8 00 00 00 06 48 8B CE 48 8B 06 F3 48 0F 2C E8",
            U,
            "Research spot (regprobe).",
        )
        .0,
        sig("sig.spot_range", "who-runs spot: range", "F3 0F 10 30 48 85 C9 74 30 E8", U, "Research spot (regprobe).").0,
        sig("sig.spot_regen", "who-runs spot: regen", "F3 0F 10 84 81 CC 08 00 00", U, "Research spot (regprobe).").0,
        sig("sig.spot_regen_delay", "who-runs spot: regen delay", "F3 0F 10 84 81 E8 08 00 00", U, "Research spot (regprobe).")
            .0,
        sig("sig.spot_explosive", "who-runs spot: explosive", "F3 44 0F 59 00 8B", U, "Research spot (regprobe).").0,
    ];

    db.experiments = vec![
        mk("exp.stand_modes", "stand modes A/B/B+patch/C for firing while flying", U)
            .found("fly.rs, standfilter.rs")
            .lim("Parked 1 Nov; mode 4 never reached in the player's use.")
            .0,
        mk("exp.standsearch", "snapshot body and its pointees on ground vs air, diff, then access-watch candidates", U)
            .found("standsearch.rs (dead in the mod)")
            .lim("Method for priorities 1 and 2; snapshot/diff now lives in grbresearch, watches in grbresearch-dll.")
            .0,
        mk("exp.health_probe", "values that only go down while an NPC is hurt (health candidates)", U)
            .found("dmgprobe.rs")
            .lim("Method only; no confirmed health offset.")
            .0,
        mk("exp.marker_search", "x,y pair search near the player, narrowed by moving (map marker)", U)
            .found("markerscan.rs")
            .lim("Method only; no teleport-to-marker exists.")
            .0,
    ];

    db.notes = vec![
        strong("note.ptr_check", "pointer test: 0x10000 < v < 0x7FFF_FFFF_0000, 8-aligned; position finite and |v| < 1e5", "ESP")
            .0,
        strong("note.safe_walk", "walk only committed memory, skip guard stacks and GPU memory, read only pages in RAM", "ESP scan")
            .ev("God mode once wrote into a stack and the game closed; touching reserved pages grew the game by GBs.")
            .0,
        strong("note.hud_metres", "game HUD metres = world units x 1.5", "ESP distance").0,
        strong("note.target_txt", "%APPDATA%\\Aurora\\Dumper\\target.txt holds the picked target's body address (hex)", "Target")
            .0,
        strong("note.weapon_patterns", "RECOIL/SPREAD/AMMO/SHAKE/SWAY/STAMINA/DAMAGE patterns", "Weapon")
            .lim("Patterns not copied here: see aurora-dll weapon.rs:15-34.")
            .0,
        mk("note.dx11", "GRB.exe (DX11) is a different exe", U)
            .exe_dx11()
            .lim("No address from GRB_vulkan.exe may be assumed to hold on GRB.exe.")
            .0,
    ];
    db
}

impl B {
    fn exe_dx11(mut self) -> B {
        self.0.exe = "GRB.exe".into();
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_statuses_follow_the_rules() {
        let db = seed("2026-11-03");
        let c = db.count_by_status();
        assert_eq!(c.get(&Status::Verified), None, "the seed never says verified");
        assert!(c[&Status::Strong] > 20);
        assert!(c[&Status::Unverified] > 10);
        let get = |id: &str| db.all().into_iter().find(|(_, e)| e.id == id).unwrap().1.clone();
        assert_eq!(get("off.move_state").status, Status::Rejected);
        assert!(get("off.move_state").evidence.contains("never read while flying") || get("off.move_state").evidence.contains("Never read"));
        assert_eq!(get("off.body_pos").status, Status::Strong);
        assert_eq!(get("off.cam_fov").status, Status::Unverified);
        assert_eq!(get("prof.grb_dx11").status, Status::Unverified);
        assert_eq!(get("prof.grb_dx11").exe, "GRB.exe");
        let mut ids: Vec<_> = db.all().iter().map(|(_, e)| e.id.clone()).collect();
        let n = ids.len();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), n, "ids are unique");
    }

    #[test]
    fn round_trip_find_and_edit() {
        let mut db = seed("2026-11-03");
        let mut e = Entry::default();
        for (k, v) in [("id", "off.test"), ("type", "Body"), ("offset", "0x30"), ("pattern", "AA BB"), ("status", "strong")] {
            set_field(&mut e, k, v).unwrap();
        }
        assert!(set_field(&mut e, "status", "sure").is_err());
        db.offsets.push(e);
        let text = serde_json::to_string_pretty(&db).unwrap();
        assert!(text.contains("\"pattern\": \"AA BB\""));
        let back: Db = serde_json::from_str(&text).unwrap();
        assert_eq!(back, db);
        assert_eq!(back.find("aa bb").len(), 1);
        assert!(back.find("CHARCOMPONENT").len() >= 3);
        assert!(back.find("signatures").len() >= 10);
        assert!(back.find("zzzz-nothing").is_empty());
        // Hand-edited files may leave fields out.
        let partial: Db = serde_json::from_str(r#"{"notes":[{"id":"n1","status":"verified"}]}"#).unwrap();
        assert_eq!(partial.notes[0].status, Status::Verified);
        assert_eq!(offset_value("0x20"), Some(0x20));
        assert_eq!(offset_value("+0x60 -> +0x160"), None);
    }

    #[test]
    fn saves_and_seeds_once() {
        let dir = crate::store::test_dir("db");
        let path = dir.join("research.json");
        let _ = std::fs::remove_file(&path);
        let mut db = Db::open(&path, "2026-11-03").unwrap();
        db.notes.push(Entry { id: "note.mine".into(), ..Default::default() });
        db.save(&path).unwrap();
        let again = Db::open(&path, "2026-11-04").unwrap();
        assert!(again.notes.iter().any(|e| e.id == "note.mine"), "existing file is not re-seeded");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
