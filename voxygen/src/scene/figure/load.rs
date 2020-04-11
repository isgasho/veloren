use crate::{
    mesh::Meshable,
    render::{FigurePipeline, Mesh},
};
use common::{
    assets::{self, watch::ReloadIndicator, Asset},
    comp::{
        biped_large::{BodyType as BLBodyType, Species as BLSpecies},
        bird_medium::{BodyType as BMBodyType, Species as BMSpecies},
        bird_small,
        critter::{BodyType as CBodyType, Species as CSpecies},
        dragon, fish_medium, fish_small,
        humanoid::{Body, BodyType, EyeColor, Eyebrows, Race, Skin},
        item::{
            armor::{Armor, Back, Belt, Chest, Foot, Hand, Head, Pants, Shoulder, Tabard},
            tool::{Tool, ToolKind},
            ItemKind, Lantern,
        },
        object,
        quadruped_medium::{BodyType as QMBodyType, Species as QMSpecies},
        quadruped_small::{BodyType as QSBodyType, Species as QSSpecies},
        Loadout,
    },
    figure::{DynaUnionizer, MatSegment, Material, Segment},
};
use dot_vox::DotVoxData;
use hashbrown::HashMap;
use log::{error, warn};
use serde_derive::{Deserialize, Serialize};
use std::{fs::File, io::BufReader, sync::Arc};
use vek::*;

fn load_segment(mesh_name: &str) -> Segment {
    let full_specifier: String = ["voxygen.voxel.", mesh_name].concat();
    Segment::from(assets::load_expect::<DotVoxData>(full_specifier.as_str()).as_ref())
}
fn graceful_load_vox(mesh_name: &str) -> Arc<DotVoxData> {
    let full_specifier: String = ["voxygen.voxel.", mesh_name].concat();
    match assets::load::<DotVoxData>(full_specifier.as_str()) {
        Ok(dot_vox) => dot_vox,
        Err(_) => {
            error!("Could not load vox file for figure: {}", full_specifier);
            assets::load_expect::<DotVoxData>("voxygen.voxel.not_found")
        },
    }
}
fn graceful_load_segment(mesh_name: &str) -> Segment {
    Segment::from(graceful_load_vox(mesh_name).as_ref())
}
fn graceful_load_mat_segment(mesh_name: &str) -> MatSegment {
    MatSegment::from(graceful_load_vox(mesh_name).as_ref())
}
fn graceful_load_mat_segment_flipped(mesh_name: &str) -> MatSegment {
    MatSegment::from_vox(graceful_load_vox(mesh_name).as_ref(), true)
}

fn generate_mesh(segment: &Segment, offset: Vec3<f32>) -> Mesh<FigurePipeline> {
    Meshable::<FigurePipeline, FigurePipeline>::generate_mesh(segment, offset).0
}

pub fn load_mesh(mesh_name: &str, position: Vec3<f32>) -> Mesh<FigurePipeline> {
    generate_mesh(&load_segment(mesh_name), position)
}

fn color_segment(
    mat_segment: MatSegment,
    skin: Skin,
    hair_color: Rgb<u8>,
    eye_color: EyeColor,
) -> Segment {
    // TODO move some of the colors to common
    mat_segment.to_segment(|mat| match mat {
        Material::Skin => skin.rgb(),
        Material::SkinDark => skin.dark_rgb(),
        Material::SkinLight => skin.light_rgb(),
        Material::Hair => hair_color,
        // TODO add back multiple colors
        Material::EyeLight => eye_color.light_rgb(),
        Material::EyeDark => eye_color.dark_rgb(),
        Material::EyeWhite => eye_color.white_rgb(),
    })
}

fn recolor_grey(rgb: Rgb<u8>, color: Rgb<u8>) -> Rgb<u8> {
    use common::util::{linear_to_srgb, srgb_to_linear};

    const BASE_GREY: f32 = 178.0;
    if rgb.r == rgb.g && rgb.g == rgb.b {
        let c1 = srgb_to_linear(rgb.map(|e| e as f32 / BASE_GREY));
        let c2 = srgb_to_linear(color.map(|e| e as f32 / 255.0));

        linear_to_srgb(c1 * c2).map(|e| (e.min(1.0).max(0.0) * 255.0) as u8)
    } else {
        rgb
    }
}

// All offsets should be relative to an initial origin that doesn't change when
// combining segments
#[derive(Serialize, Deserialize)]
struct VoxSpec<T>(String, [T; 3]);

#[derive(Serialize, Deserialize)]
struct VoxSimple(String);

#[derive(Serialize, Deserialize)]
struct ArmorVoxSpec {
    vox_spec: VoxSpec<f32>,
    color: Option<[u8; 3]>,
}

// For use by armor with a left and right component
#[derive(Serialize, Deserialize)]
struct SidedArmorVoxSpec {
    left: ArmorVoxSpec,
    right: ArmorVoxSpec,
}

#[derive(Serialize, Deserialize)]
struct MobSidedVoxSpec {
    left: ArmorVoxSpec,
    right: ArmorVoxSpec,
}

// All reliant on humanoid::Race and humanoid::BodyType
#[derive(Serialize, Deserialize)]
struct HumHeadSubSpec {
    offset: [f32; 3], // Should be relative to initial origin
    head: VoxSpec<i32>,
    eyes: VoxSpec<i32>,
    hair: Vec<Option<VoxSpec<i32>>>,
    beard: Vec<Option<VoxSpec<i32>>>,
    accessory: Vec<Option<VoxSpec<i32>>>,
}
#[derive(Serialize, Deserialize)]
pub struct HumHeadSpec(HashMap<(Race, BodyType), HumHeadSubSpec>);

impl Asset for HumHeadSpec {
    const ENDINGS: &'static [&'static str] = &["ron"];

    fn parse(buf_reader: BufReader<File>) -> Result<Self, assets::Error> {
        ron::de::from_reader(buf_reader).map_err(assets::Error::parse_error)
    }
}

impl HumHeadSpec {
    pub fn load_watched(indicator: &mut ReloadIndicator) -> Arc<Self> {
        assets::load_watched::<Self>("voxygen.voxel.humanoid_head_manifest", indicator).unwrap()
    }

    pub fn mesh_head(
        &self,
        race: Race,
        body_type: BodyType,
        hair_color: u8,
        hair_style: u8,
        beard: u8,
        eye_color: u8,
        skin: u8,
        _eyebrows: Eyebrows,
        accessory: u8,
    ) -> Mesh<FigurePipeline> {
        let spec = match self.0.get(&(race, body_type)) {
            Some(spec) => spec,
            None => {
                error!(
                    "No head specification exists for the combination of {:?} and {:?}",
                    race, body_type
                );
                return load_mesh("not_found", Vec3::new(-5.0, -5.0, -2.5));
            },
        };

        let hair_rgb = race.hair_color(hair_color);
        let skin = race.skin_color(skin);
        let eye_rgb = race.eye_color(eye_color);

        // Load segment pieces
        let bare_head = graceful_load_mat_segment(&spec.head.0);
        let eyes = color_segment(
            graceful_load_mat_segment(&spec.eyes.0).map_rgb(|rgb| recolor_grey(rgb, hair_rgb)),
            skin,
            hair_rgb,
            eye_rgb,
        );
        let hair = match spec.hair.get(hair_style as usize) {
            Some(Some(spec)) => Some((
                graceful_load_segment(&spec.0).map_rgb(|rgb| recolor_grey(rgb, hair_rgb)),
                Vec3::from(spec.1),
            )),
            Some(None) => None,
            None => {
                warn!("No specification for hair style {}", hair_style);
                None
            },
        };
        let beard = match spec.beard.get(beard as usize) {
            Some(Some(spec)) => Some((
                graceful_load_segment(&spec.0).map_rgb(|rgb| recolor_grey(rgb, hair_rgb)),
                Vec3::from(spec.1),
            )),
            Some(None) => None,
            None => {
                warn!("No specification for this beard: {:?}", beard);
                None
            },
        };
        let accessory = match spec.accessory.get(accessory as usize) {
            Some(Some(spec)) => Some((graceful_load_segment(&spec.0), Vec3::from(spec.1))),
            Some(None) => None,
            None => {
                warn!("No specification for this accessory: {:?}", accessory);
                None
            },
        };

        let (head, origin_offset) = DynaUnionizer::new()
            .add(
                color_segment(bare_head, skin, hair_rgb, eye_rgb),
                spec.head.1.into(),
            )
            .add(eyes, spec.eyes.1.into())
            .maybe_add(hair)
            .maybe_add(beard)
            .maybe_add(accessory)
            .unify();

        generate_mesh(
            &head,
            Vec3::from(spec.offset) + origin_offset.map(|e| e as f32 * -1.0),
        )
    }
}
// Armor spects should be in the same order, top to bottom.
// These seem overly split up, but wanted to keep the armor seperated
// unlike head which is done above.
#[derive(Serialize, Deserialize)]
pub struct ArmorVoxSpecMap<K, S>
where
    K: std::hash::Hash + std::cmp::Eq,
{
    default: S,
    map: HashMap<K, S>,
}
#[derive(Serialize, Deserialize)]
pub struct HumArmorShoulderSpec(ArmorVoxSpecMap<Shoulder, SidedArmorVoxSpec>);
#[derive(Serialize, Deserialize)]
pub struct HumArmorChestSpec(ArmorVoxSpecMap<Chest, ArmorVoxSpec>);
#[derive(Serialize, Deserialize)]
pub struct HumArmorHandSpec(ArmorVoxSpecMap<Hand, SidedArmorVoxSpec>);
#[derive(Serialize, Deserialize)]
pub struct HumArmorBeltSpec(ArmorVoxSpecMap<Belt, ArmorVoxSpec>);
#[derive(Serialize, Deserialize)]
pub struct HumArmorBackSpec(ArmorVoxSpecMap<Back, ArmorVoxSpec>);
#[derive(Serialize, Deserialize)]
pub struct HumArmorPantsSpec(ArmorVoxSpecMap<Pants, ArmorVoxSpec>);
#[derive(Serialize, Deserialize)]
pub struct HumArmorFootSpec(ArmorVoxSpecMap<Foot, ArmorVoxSpec>);
#[derive(Serialize, Deserialize)]
pub struct HumMainWeaponSpec(HashMap<ToolKind, ArmorVoxSpec>);
#[derive(Serialize, Deserialize)]
pub struct HumArmorLanternSpec(ArmorVoxSpecMap<Lantern, ArmorVoxSpec>);
#[derive(Serialize, Deserialize)]
pub struct HumArmorHeadSpec(ArmorVoxSpecMap<Head, ArmorVoxSpec>);
#[derive(Serialize, Deserialize)]
pub struct HumArmorTabardSpec(ArmorVoxSpecMap<Tabard, ArmorVoxSpec>);

impl Asset for HumArmorShoulderSpec {
    const ENDINGS: &'static [&'static str] = &["ron"];

    fn parse(buf_reader: BufReader<File>) -> Result<Self, assets::Error> {
        ron::de::from_reader(buf_reader).map_err(assets::Error::parse_error)
    }
}
impl Asset for HumArmorChestSpec {
    const ENDINGS: &'static [&'static str] = &["ron"];

    fn parse(buf_reader: BufReader<File>) -> Result<Self, assets::Error> {
        ron::de::from_reader(buf_reader).map_err(assets::Error::parse_error)
    }
}
impl Asset for HumArmorHandSpec {
    const ENDINGS: &'static [&'static str] = &["ron"];

    fn parse(buf_reader: BufReader<File>) -> Result<Self, assets::Error> {
        ron::de::from_reader(buf_reader).map_err(assets::Error::parse_error)
    }
}
impl Asset for HumArmorBeltSpec {
    const ENDINGS: &'static [&'static str] = &["ron"];

    fn parse(buf_reader: BufReader<File>) -> Result<Self, assets::Error> {
        ron::de::from_reader(buf_reader).map_err(assets::Error::parse_error)
    }
}
impl Asset for HumArmorBackSpec {
    const ENDINGS: &'static [&'static str] = &["ron"];

    fn parse(buf_reader: BufReader<File>) -> Result<Self, assets::Error> {
        ron::de::from_reader(buf_reader).map_err(assets::Error::parse_error)
    }
}
impl Asset for HumArmorPantsSpec {
    const ENDINGS: &'static [&'static str] = &["ron"];

    fn parse(buf_reader: BufReader<File>) -> Result<Self, assets::Error> {
        ron::de::from_reader(buf_reader).map_err(assets::Error::parse_error)
    }
}
impl Asset for HumArmorFootSpec {
    const ENDINGS: &'static [&'static str] = &["ron"];

    fn parse(buf_reader: BufReader<File>) -> Result<Self, assets::Error> {
        ron::de::from_reader(buf_reader).map_err(assets::Error::parse_error)
    }
}
impl Asset for HumArmorLanternSpec {
    const ENDINGS: &'static [&'static str] = &["ron"];

    fn parse(buf_reader: BufReader<File>) -> Result<Self, assets::Error> {
        ron::de::from_reader(buf_reader).map_err(assets::Error::parse_error)
    }
}
impl Asset for HumArmorHeadSpec {
    const ENDINGS: &'static [&'static str] = &["ron"];

    fn parse(buf_reader: BufReader<File>) -> Result<Self, assets::Error> {
        ron::de::from_reader(buf_reader).map_err(assets::Error::parse_error)
    }
}
impl Asset for HumArmorTabardSpec {
    const ENDINGS: &'static [&'static str] = &["ron"];

    fn parse(buf_reader: BufReader<File>) -> Result<Self, assets::Error> {
        ron::de::from_reader(buf_reader).map_err(assets::Error::parse_error)
    }
}
impl Asset for HumMainWeaponSpec {
    const ENDINGS: &'static [&'static str] = &["ron"];

    fn parse(buf_reader: BufReader<File>) -> Result<Self, assets::Error> {
        ron::de::from_reader(buf_reader).map_err(assets::Error::parse_error)
    }
}
// Shoulder
impl HumArmorShoulderSpec {
    pub fn load_watched(indicator: &mut ReloadIndicator) -> Arc<Self> {
        assets::load_watched::<Self>("voxygen.voxel.humanoid_armor_shoulder_manifest", indicator)
            .unwrap()
    }

    fn mesh_shoulder(&self, body: &Body, loadout: &Loadout, flipped: bool) -> Mesh<FigurePipeline> {
        let spec = if let Some(ItemKind::Armor {
            kind: Armor::Shoulder(shoulder),
            ..
        }) = loadout.shoulder.as_ref().map(|i| &i.kind)
        {
            match self.0.map.get(&shoulder) {
                Some(spec) => spec,
                None => {
                    error!("No shoulder specification exists for {:?}", shoulder);
                    return load_mesh("not_found", Vec3::new(-3.0, -3.5, 0.1));
                },
            }
        } else {
            &self.0.default
        };

        let shoulder_segment = color_segment(
            if flipped {
                graceful_load_mat_segment_flipped(&spec.left.vox_spec.0)
            } else {
                graceful_load_mat_segment(&spec.right.vox_spec.0)
            },
            body.race.skin_color(body.skin),
            body.race.hair_color(body.hair_color),
            body.race.eye_color(body.eye_color),
        );

        // TODO: use this if we can
        /*let mut offset = spec.vox_spec.1;
        if flipped {
            offset[0] = -(shoulder_segment.size().x as f32) - offset[0];
        }*/
        let offset = if flipped {
            spec.left.vox_spec.1
        } else {
            spec.right.vox_spec.1
        };

        generate_mesh(&shoulder_segment, Vec3::from(offset))
    }

    pub fn mesh_left_shoulder(&self, body: &Body, loadout: &Loadout) -> Mesh<FigurePipeline> {
        self.mesh_shoulder(body, loadout, true)
    }

    pub fn mesh_right_shoulder(&self, body: &Body, loadout: &Loadout) -> Mesh<FigurePipeline> {
        self.mesh_shoulder(body, loadout, false)
    }
}
// Chest
impl HumArmorChestSpec {
    pub fn load_watched(indicator: &mut ReloadIndicator) -> Arc<Self> {
        assets::load_watched::<Self>("voxygen.voxel.humanoid_armor_chest_manifest", indicator)
            .unwrap()
    }

    pub fn mesh_chest(&self, body: &Body, loadout: &Loadout) -> Mesh<FigurePipeline> {
        let spec = if let Some(ItemKind::Armor {
            kind: Armor::Chest(chest),
            ..
        }) = loadout.chest.as_ref().map(|i| &i.kind)
        {
            match self.0.map.get(&chest) {
                Some(spec) => spec,
                None => {
                    error!("No chest specification exists for {:?}", loadout.chest);
                    return load_mesh("not_found", Vec3::new(-7.0, -3.5, 2.0));
                },
            }
        } else {
            &self.0.default
        };

        let color = |mat_segment| {
            color_segment(
                mat_segment,
                body.race.skin_color(body.skin),
                body.race.hair_color(body.hair_color),
                body.race.eye_color(body.eye_color),
            )
        };

        let bare_chest = graceful_load_mat_segment("armor.empty");

        let mut chest_armor = graceful_load_mat_segment(&spec.vox_spec.0);

        if let Some(color) = spec.color {
            let chest_color = Vec3::from(color);
            chest_armor = chest_armor.map_rgb(|rgb| recolor_grey(rgb, Rgb::from(chest_color)));
        }

        let chest = DynaUnionizer::new()
            .add(color(bare_chest), Vec3::new(0, 0, 0))
            .add(color(chest_armor), Vec3::new(0, 0, 0))
            .unify()
            .0;

        generate_mesh(&chest, Vec3::from(spec.vox_spec.1))
    }
}
// Hand
impl HumArmorHandSpec {
    pub fn load_watched(indicator: &mut ReloadIndicator) -> Arc<Self> {
        assets::load_watched::<Self>("voxygen.voxel.humanoid_armor_hand_manifest", indicator)
            .unwrap()
    }

    fn mesh_hand(&self, body: &Body, loadout: &Loadout, flipped: bool) -> Mesh<FigurePipeline> {
        let spec = if let Some(ItemKind::Armor {
            kind: Armor::Hand(hand),
            ..
        }) = loadout.hand.as_ref().map(|i| &i.kind)
        {
            match self.0.map.get(&hand) {
                Some(spec) => spec,
                None => {
                    error!("No hand specification exists for {:?}", hand);
                    return load_mesh("not_found", Vec3::new(-1.5, -1.5, -7.0));
                },
            }
        } else {
            &self.0.default
        };

        let hand_segment = color_segment(
            if flipped {
                graceful_load_mat_segment_flipped(&spec.left.vox_spec.0)
            } else {
                graceful_load_mat_segment(&spec.right.vox_spec.0)
            },
            body.race.skin_color(body.skin),
            body.race.hair_color(body.hair_color),
            body.race.eye_color(body.eye_color),
        );

        let offset = if flipped {
            spec.left.vox_spec.1
        } else {
            spec.right.vox_spec.1
        };

        generate_mesh(&hand_segment, Vec3::from(offset))
    }

    pub fn mesh_left_hand(&self, body: &Body, loadout: &Loadout) -> Mesh<FigurePipeline> {
        self.mesh_hand(body, loadout, true)
    }

    pub fn mesh_right_hand(&self, body: &Body, loadout: &Loadout) -> Mesh<FigurePipeline> {
        self.mesh_hand(body, loadout, false)
    }
}
// Belt
impl HumArmorBeltSpec {
    pub fn load_watched(indicator: &mut ReloadIndicator) -> Arc<Self> {
        assets::load_watched::<Self>("voxygen.voxel.humanoid_armor_belt_manifest", indicator)
            .unwrap()
    }

    pub fn mesh_belt(&self, body: &Body, loadout: &Loadout) -> Mesh<FigurePipeline> {
        let spec = if let Some(ItemKind::Armor {
            kind: Armor::Belt(belt),
            ..
        }) = loadout.belt.as_ref().map(|i| &i.kind)
        {
            match self.0.map.get(&belt) {
                Some(spec) => spec,
                None => {
                    error!("No belt specification exists for {:?}", belt);
                    return load_mesh("not_found", Vec3::new(-4.0, -3.5, 2.0));
                },
            }
        } else {
            &self.0.default
        };

        let belt_segment = color_segment(
            graceful_load_mat_segment(&spec.vox_spec.0),
            body.race.skin_color(body.skin),
            body.race.hair_color(body.hair_color),
            body.race.eye_color(body.eye_color),
        );

        generate_mesh(&belt_segment, Vec3::from(spec.vox_spec.1))
    }
}
// Cape
impl HumArmorBackSpec {
    pub fn load_watched(indicator: &mut ReloadIndicator) -> Arc<Self> {
        assets::load_watched::<Self>("voxygen.voxel.humanoid_armor_back_manifest", indicator)
            .unwrap()
    }

    pub fn mesh_back(&self, body: &Body, loadout: &Loadout) -> Mesh<FigurePipeline> {
        let spec = if let Some(ItemKind::Armor {
            kind: Armor::Back(back),
            ..
        }) = loadout.back.as_ref().map(|i| &i.kind)
        {
            match self.0.map.get(&back) {
                Some(spec) => spec,
                None => {
                    error!("No back specification exists for {:?}", back);
                    return load_mesh("not_found", Vec3::new(-4.0, -3.5, 2.0));
                },
            }
        } else {
            &self.0.default
        };

        let back_segment = color_segment(
            graceful_load_mat_segment(&spec.vox_spec.0),
            body.race.skin_color(body.skin),
            body.race.hair_color(body.hair_color),
            body.race.eye_color(body.eye_color),
        );

        generate_mesh(&back_segment, Vec3::from(spec.vox_spec.1))
    }
}
// Legs
impl HumArmorPantsSpec {
    pub fn load_watched(indicator: &mut ReloadIndicator) -> Arc<Self> {
        assets::load_watched::<Self>("voxygen.voxel.humanoid_armor_pants_manifest", indicator)
            .unwrap()
    }

    pub fn mesh_pants(&self, body: &Body, loadout: &Loadout) -> Mesh<FigurePipeline> {
        let spec = if let Some(ItemKind::Armor {
            kind: Armor::Pants(pants),
            ..
        }) = loadout.pants.as_ref().map(|i| &i.kind)
        {
            match self.0.map.get(&pants) {
                Some(spec) => spec,
                None => {
                    error!("No pants specification exists for {:?}", pants);
                    return load_mesh("not_found", Vec3::new(-5.0, -3.5, 1.0));
                },
            }
        } else {
            &self.0.default
        };

        let color = |mat_segment| {
            color_segment(
                mat_segment,
                body.race.skin_color(body.skin),
                body.race.hair_color(body.hair_color),
                body.race.eye_color(body.eye_color),
            )
        };

        let bare_pants = graceful_load_mat_segment("armor.empty");

        let mut pants_armor = graceful_load_mat_segment(&spec.vox_spec.0);

        if let Some(color) = spec.color {
            let pants_color = Vec3::from(color);
            pants_armor = pants_armor.map_rgb(|rgb| recolor_grey(rgb, Rgb::from(pants_color)));
        }

        let pants = DynaUnionizer::new()
            .add(color(bare_pants), Vec3::new(0, 0, 0))
            .add(color(pants_armor), Vec3::new(0, 0, 0))
            .unify()
            .0;

        generate_mesh(&pants, Vec3::from(spec.vox_spec.1))
    }
}
// Foot
impl HumArmorFootSpec {
    pub fn load_watched(indicator: &mut ReloadIndicator) -> Arc<Self> {
        assets::load_watched::<Self>("voxygen.voxel.humanoid_armor_foot_manifest", indicator)
            .unwrap()
    }

    fn mesh_foot(&self, body: &Body, loadout: &Loadout, flipped: bool) -> Mesh<FigurePipeline> {
        let spec = if let Some(ItemKind::Armor {
            kind: Armor::Foot(foot),
            ..
        }) = loadout.foot.as_ref().map(|i| &i.kind)
        {
            match self.0.map.get(&foot) {
                Some(spec) => spec,
                None => {
                    error!("No foot specification exists for {:?}", foot);
                    return load_mesh("not_found", Vec3::new(-2.5, -3.5, -9.0));
                },
            }
        } else {
            &self.0.default
        };

        let foot_segment = color_segment(
            if flipped {
                graceful_load_mat_segment_flipped(&spec.vox_spec.0)
            } else {
                graceful_load_mat_segment(&spec.vox_spec.0)
            },
            body.race.skin_color(body.skin),
            body.race.hair_color(body.hair_color),
            body.race.eye_color(body.eye_color),
        );

        generate_mesh(&foot_segment, Vec3::from(spec.vox_spec.1))
    }

    pub fn mesh_left_foot(&self, body: &Body, loadout: &Loadout) -> Mesh<FigurePipeline> {
        self.mesh_foot(body, loadout, true)
    }

    pub fn mesh_right_foot(&self, body: &Body, loadout: &Loadout) -> Mesh<FigurePipeline> {
        self.mesh_foot(body, loadout, false)
    }
}

impl HumMainWeaponSpec {
    pub fn load_watched(indicator: &mut ReloadIndicator) -> Arc<Self> {
        assets::load_watched::<Self>("voxygen.voxel.humanoid_main_weapon_manifest", indicator)
            .unwrap()
    }

    pub fn mesh_main_weapon(&self, item_kind: Option<&ItemKind>) -> Mesh<FigurePipeline> {
        let tool_kind = if let Some(ItemKind::Tool(Tool { kind, .. })) = item_kind {
            kind
        } else {
            return Mesh::new();
        };

        let spec = match self.0.get(tool_kind) {
            Some(spec) => spec,
            None => {
                error!("No hand specification exists for {:?}", tool_kind);
                return load_mesh("not_found", Vec3::new(-1.5, -1.5, -7.0));
            },
        };

        let tool_kind_segment = graceful_load_segment(&spec.vox_spec.0);
        generate_mesh(&tool_kind_segment, Vec3::from(spec.vox_spec.1))
    }
}
// Lantern
impl HumArmorLanternSpec {
    pub fn load_watched(indicator: &mut ReloadIndicator) -> Arc<Self> {
        assets::load_watched::<Self>("voxygen.voxel.humanoid_lantern_manifest", indicator).unwrap()
    }

    pub fn mesh_lantern(&self, body: &Body, loadout: &Loadout) -> Mesh<FigurePipeline> {
        let spec =
            if let Some(ItemKind::Lantern(lantern)) = loadout.lantern.as_ref().map(|i| &i.kind) {
                match self.0.map.get(&lantern) {
                    Some(spec) => spec,
                    None => {
                        error!("No lantern specification exists for {:?}", lantern);
                        return load_mesh("not_found", Vec3::new(-4.0, -3.5, 2.0));
                    },
                }
            } else {
                &self.0.default
            };

        let lantern_segment = color_segment(
            graceful_load_mat_segment(&spec.vox_spec.0),
            body.race.skin_color(body.skin),
            body.race.hair_color(body.hair_color),
            body.race.eye_color(body.eye_color),
        );

        generate_mesh(&lantern_segment, Vec3::from(spec.vox_spec.1))
    }
}
impl HumArmorHeadSpec {
    pub fn load_watched(indicator: &mut ReloadIndicator) -> Arc<Self> {
        assets::load_watched::<Self>("voxygen.voxel.humanoid_armor_head_manifest", indicator)
            .unwrap()
    }

    pub fn mesh_head(&self, body: &Body, loadout: &Loadout) -> Mesh<FigurePipeline> {
        let spec = if let Some(ItemKind::Armor {
            kind: Armor::Head(head),
            ..
        }) = loadout.head.as_ref().map(|i| &i.kind)
        {
            match self.0.map.get(&head) {
                Some(spec) => spec,
                None => {
                    error!("No head specification exists for {:?}", head);
                    return load_mesh("not_found", Vec3::new(-5.0, -3.5, 1.0));
                },
            }
        } else {
            &self.0.default
        };

        let color = |mat_segment| {
            color_segment(
                mat_segment,
                body.race.skin_color(body.skin),
                body.race.hair_color(body.hair_color),
                body.race.eye_color(body.eye_color),
            )
        };

        let bare_head = graceful_load_mat_segment("armor.empty");

        let mut head_armor = graceful_load_mat_segment(&spec.vox_spec.0);

        if let Some(color) = spec.color {
            let head_color = Vec3::from(color);
            head_armor = head_armor.map_rgb(|rgb| recolor_grey(rgb, Rgb::from(head_color)));
        }

        let head = DynaUnionizer::new()
            .add(color(bare_head), Vec3::new(0, 0, 0))
            .add(color(head_armor), Vec3::new(0, 0, 0))
            .unify()
            .0;

        generate_mesh(&head, Vec3::from(spec.vox_spec.1))
    }
}
impl HumArmorTabardSpec {
    pub fn load_watched(indicator: &mut ReloadIndicator) -> Arc<Self> {
        assets::load_watched::<Self>("voxygen.voxel.humanoid_armor_tabard_manifest", indicator)
            .unwrap()
    }

    pub fn mesh_tabard(&self, body: &Body, loadout: &Loadout) -> Mesh<FigurePipeline> {
        let spec = if let Some(ItemKind::Armor {
            kind: Armor::Tabard(tabard),
            ..
        }) = loadout.tabard.as_ref().map(|i| &i.kind)
        {
            match self.0.map.get(&tabard) {
                Some(spec) => spec,
                None => {
                    error!("No tabard specification exists for {:?}", tabard);
                    return load_mesh("not_found", Vec3::new(-5.0, -3.5, 1.0));
                },
            }
        } else {
            &self.0.default
        };

        let color = |mat_segment| {
            color_segment(
                mat_segment,
                body.race.skin_color(body.skin),
                body.race.hair_color(body.hair_color),
                body.race.eye_color(body.eye_color),
            )
        };

        let bare_tabard = graceful_load_mat_segment("armor.empty");

        let mut tabard_armor = graceful_load_mat_segment(&spec.vox_spec.0);

        if let Some(color) = spec.color {
            let tabard_color = Vec3::from(color);
            tabard_armor = tabard_armor.map_rgb(|rgb| recolor_grey(rgb, Rgb::from(tabard_color)));
        }

        let tabard = DynaUnionizer::new()
            .add(color(bare_tabard), Vec3::new(0, 0, 0))
            .add(color(tabard_armor), Vec3::new(0, 0, 0))
            .unify()
            .0;

        generate_mesh(&tabard, Vec3::from(spec.vox_spec.1))
    }
}
// TODO: Inventory
pub fn mesh_glider() -> Mesh<FigurePipeline> {
    load_mesh("object.glider", Vec3::new(-26.0, -26.0, -5.0))
}

/////////
#[derive(Serialize, Deserialize)]
pub struct QuadrupedSmallCentralSpec(HashMap<(QSSpecies, QSBodyType), SidedQSCentralVoxSpec>);

#[derive(Serialize, Deserialize)]
struct SidedQSCentralVoxSpec {
    head: QuadrupedSmallCentralSubSpec,
    chest: QuadrupedSmallCentralSubSpec,
}
#[derive(Serialize, Deserialize)]
struct QuadrupedSmallCentralSubSpec {
    offset: [f32; 3], // Should be relative to initial origin
    central: VoxSimple,
}

#[derive(Serialize, Deserialize)]
pub struct QuadrupedSmallLateralSpec(HashMap<(QSSpecies, QSBodyType), SidedQSLateralVoxSpec>);

#[derive(Serialize, Deserialize)]
struct SidedQSLateralVoxSpec {
    left_front: QuadrupedSmallLateralSubSpec,
    right_front: QuadrupedSmallLateralSubSpec,
    left_back: QuadrupedSmallLateralSubSpec,
    right_back: QuadrupedSmallLateralSubSpec,
}
#[derive(Serialize, Deserialize)]
struct QuadrupedSmallLateralSubSpec {
    offset: [f32; 3], // Should be relative to initial origin
    lateral: VoxSimple,
}

impl Asset for QuadrupedSmallCentralSpec {
    const ENDINGS: &'static [&'static str] = &["ron"];

    fn parse(buf_reader: BufReader<File>) -> Result<Self, assets::Error> {
        ron::de::from_reader(buf_reader).map_err(assets::Error::parse_error)
    }
}

impl Asset for QuadrupedSmallLateralSpec {
    const ENDINGS: &'static [&'static str] = &["ron"];

    fn parse(buf_reader: BufReader<File>) -> Result<Self, assets::Error> {
        ron::de::from_reader(buf_reader).map_err(assets::Error::parse_error)
    }
}

impl QuadrupedSmallCentralSpec {
    pub fn load_watched(indicator: &mut ReloadIndicator) -> Arc<Self> {
        assets::load_watched::<Self>("voxygen.voxel.quadruped_small_central_manifest", indicator)
            .unwrap()
    }

    pub fn mesh_head(&self, species: QSSpecies, body_type: QSBodyType) -> Mesh<FigurePipeline> {
        let spec = match self.0.get(&(species, body_type)) {
            Some(spec) => spec,
            None => {
                error!(
                    "No head specification exists for the combination of {:?} and {:?}",
                    species, body_type
                );
                return load_mesh("not_found", Vec3::new(-5.0, -5.0, -2.5));
            },
        };
        let central = graceful_load_segment(&spec.head.central.0);

        generate_mesh(&central, Vec3::from(spec.head.offset))
    }

    pub fn mesh_chest(&self, species: QSSpecies, body_type: QSBodyType) -> Mesh<FigurePipeline> {
        let spec = match self.0.get(&(species, body_type)) {
            Some(spec) => spec,
            None => {
                error!(
                    "No chest specification exists for the combination of {:?} and {:?}",
                    species, body_type
                );
                return load_mesh("not_found", Vec3::new(-5.0, -5.0, -2.5));
            },
        };
        let central = graceful_load_segment(&spec.chest.central.0);

        generate_mesh(&central, Vec3::from(spec.chest.offset))
    }
}

impl QuadrupedSmallLateralSpec {
    pub fn load_watched(indicator: &mut ReloadIndicator) -> Arc<Self> {
        assets::load_watched::<Self>("voxygen.voxel.quadruped_small_lateral_manifest", indicator)
            .unwrap()
    }

    pub fn mesh_foot_lf(&self, species: QSSpecies, body_type: QSBodyType) -> Mesh<FigurePipeline> {
        let spec = match self.0.get(&(species, body_type)) {
            Some(spec) => spec,
            None => {
                error!(
                    "No leg specification exists for the combination of {:?} and {:?}",
                    species, body_type
                );
                return load_mesh("not_found", Vec3::new(-5.0, -5.0, -2.5));
            },
        };
        let lateral = graceful_load_segment(&spec.left_front.lateral.0);

        generate_mesh(&lateral, Vec3::from(spec.left_front.offset))
    }

    pub fn mesh_foot_rf(&self, species: QSSpecies, body_type: QSBodyType) -> Mesh<FigurePipeline> {
        let spec = match self.0.get(&(species, body_type)) {
            Some(spec) => spec,
            None => {
                error!(
                    "No leg specification exists for the combination of {:?} and {:?}",
                    species, body_type
                );
                return load_mesh("not_found", Vec3::new(-5.0, -5.0, -2.5));
            },
        };
        let lateral = graceful_load_segment(&spec.right_front.lateral.0);

        generate_mesh(&lateral, Vec3::from(spec.right_front.offset))
    }

    pub fn mesh_foot_lb(&self, species: QSSpecies, body_type: QSBodyType) -> Mesh<FigurePipeline> {
        let spec = match self.0.get(&(species, body_type)) {
            Some(spec) => spec,
            None => {
                error!(
                    "No leg specification exists for the combination of {:?} and {:?}",
                    species, body_type
                );
                return load_mesh("not_found", Vec3::new(-5.0, -5.0, -2.5));
            },
        };
        let lateral = graceful_load_segment(&spec.left_back.lateral.0);

        generate_mesh(&lateral, Vec3::from(spec.left_back.offset))
    }

    pub fn mesh_foot_rb(&self, species: QSSpecies, body_type: QSBodyType) -> Mesh<FigurePipeline> {
        let spec = match self.0.get(&(species, body_type)) {
            Some(spec) => spec,
            None => {
                error!(
                    "No leg specification exists for the combination of {:?} and {:?}",
                    species, body_type
                );
                return load_mesh("not_found", Vec3::new(-5.0, -5.0, -2.5));
            },
        };
        let lateral = graceful_load_segment(&spec.right_back.lateral.0);

        generate_mesh(&lateral, Vec3::from(spec.right_back.offset))
    }
}

//////
#[derive(Serialize, Deserialize)]
pub struct QuadrupedMediumCentralSpec(HashMap<(QMSpecies, QMBodyType), SidedQMCentralVoxSpec>);

#[derive(Serialize, Deserialize)]
struct SidedQMCentralVoxSpec {
    upper: QuadrupedMediumCentralSubSpec,
    lower: QuadrupedMediumCentralSubSpec,
    jaw: QuadrupedMediumCentralSubSpec,
    ears: QuadrupedMediumCentralSubSpec,
    torso_f: QuadrupedMediumCentralSubSpec,
    torso_b: QuadrupedMediumCentralSubSpec,
    tail: QuadrupedMediumCentralSubSpec,
}
#[derive(Serialize, Deserialize)]
struct QuadrupedMediumCentralSubSpec {
    offset: [f32; 3], // Should be relative to initial origin
    central: VoxSimple,
}

#[derive(Serialize, Deserialize)]
pub struct QuadrupedMediumLateralSpec(HashMap<(QMSpecies, QMBodyType), SidedQMLateralVoxSpec>);
#[derive(Serialize, Deserialize)]
struct SidedQMLateralVoxSpec {
    left_front: QuadrupedMediumLateralSubSpec,
    right_front: QuadrupedMediumLateralSubSpec,
    left_back: QuadrupedMediumLateralSubSpec,
    right_back: QuadrupedMediumLateralSubSpec,
}
#[derive(Serialize, Deserialize)]
struct QuadrupedMediumLateralSubSpec {
    offset: [f32; 3], // Should be relative to initial origin
    lateral: VoxSimple,
}

impl Asset for QuadrupedMediumCentralSpec {
    const ENDINGS: &'static [&'static str] = &["ron"];

    fn parse(buf_reader: BufReader<File>) -> Result<Self, assets::Error> {
        ron::de::from_reader(buf_reader).map_err(assets::Error::parse_error)
    }
}

impl Asset for QuadrupedMediumLateralSpec {
    const ENDINGS: &'static [&'static str] = &["ron"];

    fn parse(buf_reader: BufReader<File>) -> Result<Self, assets::Error> {
        ron::de::from_reader(buf_reader).map_err(assets::Error::parse_error)
    }
}

impl QuadrupedMediumCentralSpec {
    pub fn load_watched(indicator: &mut ReloadIndicator) -> Arc<Self> {
        assets::load_watched::<Self>("voxygen.voxel.quadruped_medium_central_manifest", indicator)
            .unwrap()
    }

    pub fn mesh_head_upper(
        &self,
        species: QMSpecies,
        body_type: QMBodyType,
    ) -> Mesh<FigurePipeline> {
        let spec = match self.0.get(&(species, body_type)) {
            Some(spec) => spec,
            None => {
                error!(
                    "No upper head specification exists for the combination of {:?} and {:?}",
                    species, body_type
                );
                return load_mesh("not_found", Vec3::new(-5.0, -5.0, -2.5));
            },
        };
        let central = graceful_load_segment(&spec.upper.central.0);

        generate_mesh(&central, Vec3::from(spec.upper.offset))
    }

    pub fn mesh_head_lower(
        &self,
        species: QMSpecies,
        body_type: QMBodyType,
    ) -> Mesh<FigurePipeline> {
        let spec = match self.0.get(&(species, body_type)) {
            Some(spec) => spec,
            None => {
                error!(
                    "No lower head specification exists for the combination of {:?} and {:?}",
                    species, body_type
                );
                return load_mesh("not_found", Vec3::new(-5.0, -5.0, -2.5));
            },
        };
        let central = graceful_load_segment(&spec.lower.central.0);

        generate_mesh(&central, Vec3::from(spec.lower.offset))
    }

    pub fn mesh_jaw(&self, species: QMSpecies, body_type: QMBodyType) -> Mesh<FigurePipeline> {
        let spec = match self.0.get(&(species, body_type)) {
            Some(spec) => spec,
            None => {
                error!(
                    "No jaw specification exists for the combination of {:?} and {:?}",
                    species, body_type
                );
                return load_mesh("not_found", Vec3::new(-5.0, -5.0, -2.5));
            },
        };
        let central = graceful_load_segment(&spec.jaw.central.0);

        generate_mesh(&central, Vec3::from(spec.jaw.offset))
    }

    pub fn mesh_ears(&self, species: QMSpecies, body_type: QMBodyType) -> Mesh<FigurePipeline> {
        let spec = match self.0.get(&(species, body_type)) {
            Some(spec) => spec,
            None => {
                error!(
                    "No ears specification exists for the combination of {:?} and {:?}",
                    species, body_type
                );
                return load_mesh("not_found", Vec3::new(-5.0, -5.0, -2.5));
            },
        };
        let central = graceful_load_segment(&spec.ears.central.0);

        generate_mesh(&central, Vec3::from(spec.ears.offset))
    }

    pub fn mesh_torso_f(&self, species: QMSpecies, body_type: QMBodyType) -> Mesh<FigurePipeline> {
        let spec = match self.0.get(&(species, body_type)) {
            Some(spec) => spec,
            None => {
                error!(
                    "No torso specification exists for the combination of {:?} and {:?}",
                    species, body_type
                );
                return load_mesh("not_found", Vec3::new(-5.0, -5.0, -2.5));
            },
        };
        let central = graceful_load_segment(&spec.torso_f.central.0);

        generate_mesh(&central, Vec3::from(spec.torso_f.offset))
    }

    pub fn mesh_torso_b(&self, species: QMSpecies, body_type: QMBodyType) -> Mesh<FigurePipeline> {
        let spec = match self.0.get(&(species, body_type)) {
            Some(spec) => spec,
            None => {
                error!(
                    "No torso specification exists for the combination of {:?} and {:?}",
                    species, body_type
                );
                return load_mesh("not_found", Vec3::new(-5.0, -5.0, -2.5));
            },
        };
        let central = graceful_load_segment(&spec.torso_b.central.0);

        generate_mesh(&central, Vec3::from(spec.torso_b.offset))
    }

    pub fn mesh_tail(&self, species: QMSpecies, body_type: QMBodyType) -> Mesh<FigurePipeline> {
        let spec = match self.0.get(&(species, body_type)) {
            Some(spec) => spec,
            None => {
                error!(
                    "No tail specification exists for the combination of {:?} and {:?}",
                    species, body_type
                );
                return load_mesh("not_found", Vec3::new(-5.0, -5.0, -2.5));
            },
        };
        let central = graceful_load_segment(&spec.tail.central.0);

        generate_mesh(&central, Vec3::from(spec.tail.offset))
    }
}

impl QuadrupedMediumLateralSpec {
    pub fn load_watched(indicator: &mut ReloadIndicator) -> Arc<Self> {
        assets::load_watched::<Self>("voxygen.voxel.quadruped_medium_lateral_manifest", indicator)
            .unwrap()
    }

    pub fn mesh_foot_lf(&self, species: QMSpecies, body_type: QMBodyType) -> Mesh<FigurePipeline> {
        let spec = match self.0.get(&(species, body_type)) {
            Some(spec) => spec,
            None => {
                error!(
                    "No foot specification exists for the combination of {:?} and {:?}",
                    species, body_type
                );
                return load_mesh("not_found", Vec3::new(-5.0, -5.0, -2.5));
            },
        };
        let lateral = graceful_load_segment(&spec.left_front.lateral.0);

        generate_mesh(&lateral, Vec3::from(spec.left_front.offset))
    }

    pub fn mesh_foot_rf(&self, species: QMSpecies, body_type: QMBodyType) -> Mesh<FigurePipeline> {
        let spec = match self.0.get(&(species, body_type)) {
            Some(spec) => spec,
            None => {
                error!(
                    "No foot specification exists for the combination of {:?} and {:?}",
                    species, body_type
                );
                return load_mesh("not_found", Vec3::new(-5.0, -5.0, -2.5));
            },
        };
        let lateral = graceful_load_segment(&spec.right_front.lateral.0);

        generate_mesh(&lateral, Vec3::from(spec.right_front.offset))
    }

    pub fn mesh_foot_lb(&self, species: QMSpecies, body_type: QMBodyType) -> Mesh<FigurePipeline> {
        let spec = match self.0.get(&(species, body_type)) {
            Some(spec) => spec,
            None => {
                error!(
                    "No foot specification exists for the combination of {:?} and {:?}",
                    species, body_type
                );
                return load_mesh("not_found", Vec3::new(-5.0, -5.0, -2.5));
            },
        };
        let lateral = graceful_load_segment(&spec.left_back.lateral.0);

        generate_mesh(&lateral, Vec3::from(spec.left_back.offset))
    }

    pub fn mesh_foot_rb(&self, species: QMSpecies, body_type: QMBodyType) -> Mesh<FigurePipeline> {
        let spec = match self.0.get(&(species, body_type)) {
            Some(spec) => spec,
            None => {
                error!(
                    "No foot specification exists for the combination of {:?} and {:?}",
                    species, body_type
                );
                return load_mesh("not_found", Vec3::new(-5.0, -5.0, -2.5));
            },
        };
        let lateral = graceful_load_segment(&spec.right_back.lateral.0);

        generate_mesh(&lateral, Vec3::from(spec.right_back.offset))
    }
}

////
#[derive(Serialize, Deserialize)]
pub struct BirdMediumCenterSpec(HashMap<(BMSpecies, BMBodyType), SidedBMCenterVoxSpec>);

#[derive(Serialize, Deserialize)]
struct SidedBMCenterVoxSpec {
    head: BirdMediumCenterSubSpec,
    torso: BirdMediumCenterSubSpec,
    tail: BirdMediumCenterSubSpec,
}
#[derive(Serialize, Deserialize)]
struct BirdMediumCenterSubSpec {
    offset: [f32; 3], // Should be relative to initial origin
    center: VoxSimple,
}

#[derive(Serialize, Deserialize)]
pub struct BirdMediumLateralSpec(HashMap<(BMSpecies, BMBodyType), SidedBMLateralVoxSpec>);

#[derive(Serialize, Deserialize)]
struct SidedBMLateralVoxSpec {
    wing_l: BirdMediumLateralSubSpec,
    wing_r: BirdMediumLateralSubSpec,
    foot_l: BirdMediumLateralSubSpec,
    foot_r: BirdMediumLateralSubSpec,
}
#[derive(Serialize, Deserialize)]
struct BirdMediumLateralSubSpec {
    offset: [f32; 3], // Should be relative to initial origin
    lateral: VoxSimple,
}

impl Asset for BirdMediumCenterSpec {
    const ENDINGS: &'static [&'static str] = &["ron"];

    fn parse(buf_reader: BufReader<File>) -> Result<Self, assets::Error> {
        ron::de::from_reader(buf_reader).map_err(assets::Error::parse_error)
    }
}

impl Asset for BirdMediumLateralSpec {
    const ENDINGS: &'static [&'static str] = &["ron"];

    fn parse(buf_reader: BufReader<File>) -> Result<Self, assets::Error> {
        ron::de::from_reader(buf_reader).map_err(assets::Error::parse_error)
    }
}

impl BirdMediumCenterSpec {
    pub fn load_watched(indicator: &mut ReloadIndicator) -> Arc<Self> {
        assets::load_watched::<Self>("voxygen.voxel.bird_medium_center_manifest", indicator)
            .unwrap()
    }

    pub fn mesh_head(&self, species: BMSpecies, body_type: BMBodyType) -> Mesh<FigurePipeline> {
        let spec = match self.0.get(&(species, body_type)) {
            Some(spec) => spec,
            None => {
                error!(
                    "No head specification exists for the combination of {:?} and {:?}",
                    species, body_type
                );
                return load_mesh("not_found", Vec3::new(-5.0, -5.0, -2.5));
            },
        };
        let center = graceful_load_segment(&spec.head.center.0);

        generate_mesh(&center, Vec3::from(spec.head.offset))
    }

    pub fn mesh_torso(&self, species: BMSpecies, body_type: BMBodyType) -> Mesh<FigurePipeline> {
        let spec = match self.0.get(&(species, body_type)) {
            Some(spec) => spec,
            None => {
                error!(
                    "No torso specification exists for the combination of {:?} and {:?}",
                    species, body_type
                );
                return load_mesh("not_found", Vec3::new(-5.0, -5.0, -2.5));
            },
        };
        let center = graceful_load_segment(&spec.torso.center.0);

        generate_mesh(&center, Vec3::from(spec.torso.offset))
    }

    pub fn mesh_tail(&self, species: BMSpecies, body_type: BMBodyType) -> Mesh<FigurePipeline> {
        let spec = match self.0.get(&(species, body_type)) {
            Some(spec) => spec,
            None => {
                error!(
                    "No tail specification exists for the combination of {:?} and {:?}",
                    species, body_type
                );
                return load_mesh("not_found", Vec3::new(-5.0, -5.0, -2.5));
            },
        };
        let center = graceful_load_segment(&spec.tail.center.0);

        generate_mesh(&center, Vec3::from(spec.tail.offset))
    }
}
impl BirdMediumLateralSpec {
    pub fn load_watched(indicator: &mut ReloadIndicator) -> Arc<Self> {
        assets::load_watched::<Self>("voxygen.voxel.bird_medium_lateral_manifest", indicator)
            .unwrap()
    }

    pub fn mesh_wing_l(&self, species: BMSpecies, body_type: BMBodyType) -> Mesh<FigurePipeline> {
        let spec = match self.0.get(&(species, body_type)) {
            Some(spec) => spec,
            None => {
                error!(
                    "No wing specification exists for the combination of {:?} and {:?}",
                    species, body_type
                );
                return load_mesh("not_found", Vec3::new(-5.0, -5.0, -2.5));
            },
        };
        let lateral = graceful_load_segment(&spec.wing_l.lateral.0);

        generate_mesh(&lateral, Vec3::from(spec.wing_l.offset))
    }

    pub fn mesh_wing_r(&self, species: BMSpecies, body_type: BMBodyType) -> Mesh<FigurePipeline> {
        let spec = match self.0.get(&(species, body_type)) {
            Some(spec) => spec,
            None => {
                error!(
                    "No wing specification exists for the combination of {:?} and {:?}",
                    species, body_type
                );
                return load_mesh("not_found", Vec3::new(-5.0, -5.0, -2.5));
            },
        };
        let lateral = graceful_load_segment(&spec.wing_r.lateral.0);

        generate_mesh(&lateral, Vec3::from(spec.wing_r.offset))
    }

    pub fn mesh_foot_l(&self, species: BMSpecies, body_type: BMBodyType) -> Mesh<FigurePipeline> {
        let spec = match self.0.get(&(species, body_type)) {
            Some(spec) => spec,
            None => {
                error!(
                    "No foot specification exists for the combination of {:?} and {:?}",
                    species, body_type
                );
                return load_mesh("not_found", Vec3::new(-5.0, -5.0, -2.5));
            },
        };
        let lateral = graceful_load_segment(&spec.foot_l.lateral.0);

        generate_mesh(&lateral, Vec3::from(spec.foot_l.offset))
    }

    pub fn mesh_foot_r(&self, species: BMSpecies, body_type: BMBodyType) -> Mesh<FigurePipeline> {
        let spec = match self.0.get(&(species, body_type)) {
            Some(spec) => spec,
            None => {
                error!(
                    "No foot specification exists for the combination of {:?} and {:?}",
                    species, body_type
                );
                return load_mesh("not_found", Vec3::new(-5.0, -5.0, -2.5));
            },
        };
        let lateral = graceful_load_segment(&spec.foot_r.lateral.0);

        generate_mesh(&lateral, Vec3::from(spec.foot_r.offset))
    }
}
////
#[derive(Serialize, Deserialize)]
pub struct CritterCenterSpec(HashMap<(CSpecies, CBodyType), SidedCCenterVoxSpec>);

#[derive(Serialize, Deserialize)]
struct SidedCCenterVoxSpec {
    head: CritterCenterSubSpec,
    chest: CritterCenterSubSpec,
    feet_f: CritterCenterSubSpec,
    feet_b: CritterCenterSubSpec,
    tail: CritterCenterSubSpec,
}
#[derive(Serialize, Deserialize)]
struct CritterCenterSubSpec {
    offset: [f32; 3], // Should be relative to initial origin
    center: VoxSimple,
}

impl Asset for CritterCenterSpec {
    const ENDINGS: &'static [&'static str] = &["ron"];

    fn parse(buf_reader: BufReader<File>) -> Result<Self, assets::Error> {
        ron::de::from_reader(buf_reader).map_err(assets::Error::parse_error)
    }
}

impl CritterCenterSpec {
    pub fn load_watched(indicator: &mut ReloadIndicator) -> Arc<Self> {
        assets::load_watched::<Self>("voxygen.voxel.critter_center_manifest", indicator).unwrap()
    }

    pub fn mesh_head(&self, species: CSpecies, body_type: CBodyType) -> Mesh<FigurePipeline> {
        let spec = match self.0.get(&(species, body_type)) {
            Some(spec) => spec,
            None => {
                error!(
                    "No head specification exists for the combination of {:?} and {:?}",
                    species, body_type
                );
                return load_mesh("not_found", Vec3::new(-5.0, -5.0, -2.5));
            },
        };
        let center = graceful_load_segment(&spec.head.center.0);

        generate_mesh(&center, Vec3::from(spec.head.offset))
    }

    pub fn mesh_chest(&self, species: CSpecies, body_type: CBodyType) -> Mesh<FigurePipeline> {
        let spec = match self.0.get(&(species, body_type)) {
            Some(spec) => spec,
            None => {
                error!(
                    "No chest specification exists for the combination of {:?} and {:?}",
                    species, body_type
                );
                return load_mesh("not_found", Vec3::new(-5.0, -5.0, -2.5));
            },
        };
        let center = graceful_load_segment(&spec.chest.center.0);

        generate_mesh(&center, Vec3::from(spec.chest.offset))
    }

    pub fn mesh_feet_f(&self, species: CSpecies, body_type: CBodyType) -> Mesh<FigurePipeline> {
        let spec = match self.0.get(&(species, body_type)) {
            Some(spec) => spec,
            None => {
                error!(
                    "No feet specification exists for the combination of {:?} and {:?}",
                    species, body_type
                );
                return load_mesh("not_found", Vec3::new(-5.0, -5.0, -2.5));
            },
        };
        let center = graceful_load_segment(&spec.feet_f.center.0);

        generate_mesh(&center, Vec3::from(spec.feet_f.offset))
    }

    pub fn mesh_feet_b(&self, species: CSpecies, body_type: CBodyType) -> Mesh<FigurePipeline> {
        let spec = match self.0.get(&(species, body_type)) {
            Some(spec) => spec,
            None => {
                error!(
                    "No feet specification exists for the combination of {:?} and {:?}",
                    species, body_type
                );
                return load_mesh("not_found", Vec3::new(-5.0, -5.0, -2.5));
            },
        };
        let center = graceful_load_segment(&spec.feet_b.center.0);

        generate_mesh(&center, Vec3::from(spec.feet_b.offset))
    }

    pub fn mesh_tail(&self, species: CSpecies, body_type: CBodyType) -> Mesh<FigurePipeline> {
        let spec = match self.0.get(&(species, body_type)) {
            Some(spec) => spec,
            None => {
                error!(
                    "No tail specification exists for the combination of {:?} and {:?}",
                    species, body_type
                );
                return load_mesh("not_found", Vec3::new(-5.0, -5.0, -2.5));
            },
        };
        let center = graceful_load_segment(&spec.tail.center.0);

        generate_mesh(&center, Vec3::from(spec.tail.offset))
    }
}
////
pub fn mesh_fish_medium_head(head: fish_medium::Head) -> Mesh<FigurePipeline> {
    load_mesh(
        match head {
            fish_medium::Head::Default => "npc.marlin.head",
        },
        Vec3::new(-7.0, -6.0, -6.0),
    )
}

pub fn mesh_fish_medium_torso(torso: fish_medium::Torso) -> Mesh<FigurePipeline> {
    load_mesh(
        match torso {
            fish_medium::Torso::Default => "npc.marlin.torso",
        },
        Vec3::new(-7.0, -6.0, -6.0),
    )
}

pub fn mesh_fish_medium_rear(rear: fish_medium::Rear) -> Mesh<FigurePipeline> {
    load_mesh(
        match rear {
            fish_medium::Rear::Default => "npc.marlin.rear",
        },
        Vec3::new(-7.0, -6.0, -6.0),
    )
}

pub fn mesh_fish_medium_tail(tail: fish_medium::Tail) -> Mesh<FigurePipeline> {
    load_mesh(
        match tail {
            fish_medium::Tail::Default => "npc.marlin.tail",
        },
        Vec3::new(-7.0, -6.0, -6.0),
    )
}

pub fn mesh_fish_medium_fin_l(fin_l: fish_medium::FinL) -> Mesh<FigurePipeline> {
    load_mesh(
        match fin_l {
            fish_medium::FinL::Default => "npc.marlin.fin_l",
        },
        Vec3::new(-7.0, -6.0, -6.0),
    )
}

pub fn mesh_fish_medium_fin_r(fin_r: fish_medium::FinR) -> Mesh<FigurePipeline> {
    load_mesh(
        match fin_r {
            fish_medium::FinR::Default => "npc.marlin.fin_r",
        },
        Vec3::new(-7.0, -6.0, -6.0),
    )
}

pub fn mesh_dragon_head(head: dragon::Head) -> Mesh<FigurePipeline> {
    load_mesh(
        match head {
            dragon::Head::Default => "npc.dragon.head",
        },
        Vec3::new(-7.0, -6.0, -6.0),
    )
}

pub fn mesh_dragon_chest_front(chest_front: dragon::ChestFront) -> Mesh<FigurePipeline> {
    load_mesh(
        match chest_front {
            dragon::ChestFront::Default => "npc.dragon.chest_front",
        },
        Vec3::new(-7.0, -6.0, -6.0),
    )
}

pub fn mesh_dragon_chest_rear(chest_rear: dragon::ChestRear) -> Mesh<FigurePipeline> {
    load_mesh(
        match chest_rear {
            dragon::ChestRear::Default => "npc.dragon.chest_rear",
        },
        Vec3::new(-7.0, -6.0, -6.0),
    )
}

pub fn mesh_dragon_tail_front(tail_front: dragon::TailFront) -> Mesh<FigurePipeline> {
    load_mesh(
        match tail_front {
            dragon::TailFront::Default => "npc.dragon.tail_front",
        },
        Vec3::new(-7.0, -6.0, -6.0),
    )
}

pub fn mesh_dragon_tail_rear(tail_rear: dragon::TailRear) -> Mesh<FigurePipeline> {
    load_mesh(
        match tail_rear {
            dragon::TailRear::Default => "npc.dragon.tail_rear",
        },
        Vec3::new(-7.0, -6.0, -6.0),
    )
}

pub fn mesh_dragon_wing_in_l(wing_in_l: dragon::WingInL) -> Mesh<FigurePipeline> {
    load_mesh(
        match wing_in_l {
            dragon::WingInL::Default => "npc.dragon.wing_in_l",
        },
        Vec3::new(-7.0, -6.0, -6.0),
    )
}

pub fn mesh_dragon_wing_in_r(wing_in_r: dragon::WingInR) -> Mesh<FigurePipeline> {
    load_mesh(
        match wing_in_r {
            dragon::WingInR::Default => "npc.dragon.wing_in_r",
        },
        Vec3::new(-7.0, -6.0, -6.0),
    )
}

pub fn mesh_dragon_wing_out_l(wing_out_l: dragon::WingOutL) -> Mesh<FigurePipeline> {
    load_mesh(
        match wing_out_l {
            dragon::WingOutL::Default => "npc.dragon.wing_out_l",
        },
        Vec3::new(-7.0, -6.0, -6.0),
    )
}

pub fn mesh_dragon_wing_out_r(wing_out_r: dragon::WingOutR) -> Mesh<FigurePipeline> {
    load_mesh(
        match wing_out_r {
            dragon::WingOutR::Default => "npc.dragon.wing_out_r",
        },
        Vec3::new(-7.0, -6.0, -6.0),
    )
}

pub fn mesh_dragon_foot_fl(foot_fl: dragon::FootFL) -> Mesh<FigurePipeline> {
    load_mesh(
        match foot_fl {
            dragon::FootFL::Default => "npc.dragon.foot_fl",
        },
        Vec3::new(-7.0, -6.0, -6.0),
    )
}

pub fn mesh_dragon_foot_fr(foot_fr: dragon::FootFR) -> Mesh<FigurePipeline> {
    load_mesh(
        match foot_fr {
            dragon::FootFR::Default => "npc.dragon.foot_fr",
        },
        Vec3::new(-7.0, -6.0, -6.0),
    )
}

pub fn mesh_dragon_foot_bl(foot_bl: dragon::FootBL) -> Mesh<FigurePipeline> {
    load_mesh(
        match foot_bl {
            dragon::FootBL::Default => "npc.dragon.foot_bl",
        },
        Vec3::new(-7.0, -6.0, -6.0),
    )
}

pub fn mesh_dragon_foot_br(foot_br: dragon::FootBR) -> Mesh<FigurePipeline> {
    load_mesh(
        match foot_br {
            dragon::FootBR::Default => "npc.dragon.foot_br",
        },
        Vec3::new(-7.0, -6.0, -6.0),
    )
}

////
pub fn mesh_bird_small_head(head: bird_small::Head) -> Mesh<FigurePipeline> {
    load_mesh(
        match head {
            bird_small::Head::Default => "npc.crow.head",
        },
        Vec3::new(-7.0, -6.0, -6.0),
    )
}

pub fn mesh_bird_small_torso(torso: bird_small::Torso) -> Mesh<FigurePipeline> {
    load_mesh(
        match torso {
            bird_small::Torso::Default => "npc.crow.torso",
        },
        Vec3::new(-7.0, -6.0, -6.0),
    )
}

pub fn mesh_bird_small_wing_l(wing_l: bird_small::WingL) -> Mesh<FigurePipeline> {
    load_mesh(
        match wing_l {
            bird_small::WingL::Default => "npc.crow.wing_l",
        },
        Vec3::new(-7.0, -6.0, -6.0),
    )
}

pub fn mesh_bird_small_wing_r(wing_r: bird_small::WingR) -> Mesh<FigurePipeline> {
    load_mesh(
        match wing_r {
            bird_small::WingR::Default => "npc.crow.wing_r",
        },
        Vec3::new(-7.0, -6.0, -6.0),
    )
}
////
pub fn mesh_fish_small_torso(torso: fish_small::Torso) -> Mesh<FigurePipeline> {
    load_mesh(
        match torso {
            fish_small::Torso::Default => "npc.cardinalfish.torso",
        },
        Vec3::new(-7.0, -6.0, -6.0),
    )
}

pub fn mesh_fish_small_tail(tail: fish_small::Tail) -> Mesh<FigurePipeline> {
    load_mesh(
        match tail {
            fish_small::Tail::Default => "npc.cardinalfish.tail",
        },
        Vec3::new(-7.0, -6.0, -6.0),
    )
}
////
#[derive(Serialize, Deserialize)]
pub struct BipedLargeCenterSpec(HashMap<(BLSpecies, BLBodyType), SidedBLCenterVoxSpec>);

#[derive(Serialize, Deserialize)]
struct SidedBLCenterVoxSpec {
    head: BipedLargeCenterSubSpec,
    torso_upper: BipedLargeCenterSubSpec,
    torso_lower: BipedLargeCenterSubSpec,
}
#[derive(Serialize, Deserialize)]
struct BipedLargeCenterSubSpec {
    offset: [f32; 3], // Should be relative to initial origin
    center: VoxSimple,
}

#[derive(Serialize, Deserialize)]
pub struct BipedLargeLateralSpec(HashMap<(BLSpecies, BLBodyType), SidedBLLateralVoxSpec>);

#[derive(Serialize, Deserialize)]
struct SidedBLLateralVoxSpec {
    shoulder_l: BipedLargeLateralSubSpec,
    shoulder_r: BipedLargeLateralSubSpec,
    hand_l: BipedLargeLateralSubSpec,
    hand_r: BipedLargeLateralSubSpec,
    leg_l: BipedLargeLateralSubSpec,
    leg_r: BipedLargeLateralSubSpec,
    foot_l: BipedLargeLateralSubSpec,
    foot_r: BipedLargeLateralSubSpec,
}
#[derive(Serialize, Deserialize)]
struct BipedLargeLateralSubSpec {
    offset: [f32; 3], // Should be relative to initial origin
    lateral: VoxSimple,
}

impl Asset for BipedLargeCenterSpec {
    const ENDINGS: &'static [&'static str] = &["ron"];

    fn parse(buf_reader: BufReader<File>) -> Result<Self, assets::Error> {
        ron::de::from_reader(buf_reader).map_err(assets::Error::parse_error)
    }
}

impl Asset for BipedLargeLateralSpec {
    const ENDINGS: &'static [&'static str] = &["ron"];

    fn parse(buf_reader: BufReader<File>) -> Result<Self, assets::Error> {
        ron::de::from_reader(buf_reader).map_err(assets::Error::parse_error)
    }
}

impl BipedLargeCenterSpec {
    pub fn load_watched(indicator: &mut ReloadIndicator) -> Arc<Self> {
        assets::load_watched::<Self>("voxygen.voxel.biped_large_center_manifest", indicator)
            .unwrap()
    }

    pub fn mesh_head(&self, species: BLSpecies, body_type: BLBodyType) -> Mesh<FigurePipeline> {
        let spec = match self.0.get(&(species, body_type)) {
            Some(spec) => spec,
            None => {
                error!(
                    "No head specification exists for the combination of {:?} and {:?}",
                    species, body_type
                );
                return load_mesh("not_found", Vec3::new(-5.0, -5.0, -2.5));
            },
        };
        let center = graceful_load_segment(&spec.head.center.0);

        generate_mesh(&center, Vec3::from(spec.head.offset))
    }

    pub fn mesh_torso_upper(
        &self,
        species: BLSpecies,
        body_type: BLBodyType,
    ) -> Mesh<FigurePipeline> {
        let spec = match self.0.get(&(species, body_type)) {
            Some(spec) => spec,
            None => {
                error!(
                    "No torso upper specification exists for the combination of {:?} and {:?}",
                    species, body_type
                );
                return load_mesh("not_found", Vec3::new(-5.0, -5.0, -2.5));
            },
        };
        let center = graceful_load_segment(&spec.torso_upper.center.0);

        generate_mesh(&center, Vec3::from(spec.torso_upper.offset))
    }

    pub fn mesh_torso_lower(
        &self,
        species: BLSpecies,
        body_type: BLBodyType,
    ) -> Mesh<FigurePipeline> {
        let spec = match self.0.get(&(species, body_type)) {
            Some(spec) => spec,
            None => {
                error!(
                    "No torso lower specification exists for the combination of {:?} and {:?}",
                    species, body_type
                );
                return load_mesh("not_found", Vec3::new(-5.0, -5.0, -2.5));
            },
        };
        let center = graceful_load_segment(&spec.torso_lower.center.0);

        generate_mesh(&center, Vec3::from(spec.torso_lower.offset))
    }
}
impl BipedLargeLateralSpec {
    pub fn load_watched(indicator: &mut ReloadIndicator) -> Arc<Self> {
        assets::load_watched::<Self>("voxygen.voxel.biped_large_lateral_manifest", indicator)
            .unwrap()
    }

    pub fn mesh_shoulder_l(
        &self,
        species: BLSpecies,
        body_type: BLBodyType,
    ) -> Mesh<FigurePipeline> {
        let spec = match self.0.get(&(species, body_type)) {
            Some(spec) => spec,
            None => {
                error!(
                    "No shoulder specification exists for the combination of {:?} and {:?}",
                    species, body_type
                );
                return load_mesh("not_found", Vec3::new(-5.0, -5.0, -2.5));
            },
        };
        let lateral = graceful_load_segment(&spec.shoulder_l.lateral.0);

        generate_mesh(&lateral, Vec3::from(spec.shoulder_l.offset))
    }

    pub fn mesh_shoulder_r(
        &self,
        species: BLSpecies,
        body_type: BLBodyType,
    ) -> Mesh<FigurePipeline> {
        let spec = match self.0.get(&(species, body_type)) {
            Some(spec) => spec,
            None => {
                error!(
                    "No shoulder specification exists for the combination of {:?} and {:?}",
                    species, body_type
                );
                return load_mesh("not_found", Vec3::new(-5.0, -5.0, -2.5));
            },
        };
        let lateral = graceful_load_segment(&spec.shoulder_r.lateral.0);

        generate_mesh(&lateral, Vec3::from(spec.shoulder_r.offset))
    }

    pub fn mesh_hand_l(&self, species: BLSpecies, body_type: BLBodyType) -> Mesh<FigurePipeline> {
        let spec = match self.0.get(&(species, body_type)) {
            Some(spec) => spec,
            None => {
                error!(
                    "No hand specification exists for the combination of {:?} and {:?}",
                    species, body_type
                );
                return load_mesh("not_found", Vec3::new(-5.0, -5.0, -2.5));
            },
        };
        let lateral = graceful_load_segment(&spec.hand_l.lateral.0);

        generate_mesh(&lateral, Vec3::from(spec.hand_l.offset))
    }

    pub fn mesh_hand_r(&self, species: BLSpecies, body_type: BLBodyType) -> Mesh<FigurePipeline> {
        let spec = match self.0.get(&(species, body_type)) {
            Some(spec) => spec,
            None => {
                error!(
                    "No hand specification exists for the combination of {:?} and {:?}",
                    species, body_type
                );
                return load_mesh("not_found", Vec3::new(-5.0, -5.0, -2.5));
            },
        };
        let lateral = graceful_load_segment(&spec.hand_r.lateral.0);

        generate_mesh(&lateral, Vec3::from(spec.hand_r.offset))
    }

    pub fn mesh_leg_l(&self, species: BLSpecies, body_type: BLBodyType) -> Mesh<FigurePipeline> {
        let spec = match self.0.get(&(species, body_type)) {
            Some(spec) => spec,
            None => {
                error!(
                    "No leg specification exists for the combination of {:?} and {:?}",
                    species, body_type
                );
                return load_mesh("not_found", Vec3::new(-5.0, -5.0, -2.5));
            },
        };
        let lateral = graceful_load_segment(&spec.leg_l.lateral.0);

        generate_mesh(&lateral, Vec3::from(spec.leg_l.offset))
    }

    pub fn mesh_leg_r(&self, species: BLSpecies, body_type: BLBodyType) -> Mesh<FigurePipeline> {
        let spec = match self.0.get(&(species, body_type)) {
            Some(spec) => spec,
            None => {
                error!(
                    "No leg specification exists for the combination of {:?} and {:?}",
                    species, body_type
                );
                return load_mesh("not_found", Vec3::new(-5.0, -5.0, -2.5));
            },
        };
        let lateral = graceful_load_segment(&spec.leg_r.lateral.0);

        generate_mesh(&lateral, Vec3::from(spec.leg_r.offset))
    }

    pub fn mesh_foot_l(&self, species: BLSpecies, body_type: BLBodyType) -> Mesh<FigurePipeline> {
        let spec = match self.0.get(&(species, body_type)) {
            Some(spec) => spec,
            None => {
                error!(
                    "No foot specification exists for the combination of {:?} and {:?}",
                    species, body_type
                );
                return load_mesh("not_found", Vec3::new(-5.0, -5.0, -2.5));
            },
        };
        let lateral = graceful_load_segment(&spec.foot_l.lateral.0);

        generate_mesh(&lateral, Vec3::from(spec.foot_l.offset))
    }

    pub fn mesh_foot_r(&self, species: BLSpecies, body_type: BLBodyType) -> Mesh<FigurePipeline> {
        let spec = match self.0.get(&(species, body_type)) {
            Some(spec) => spec,
            None => {
                error!(
                    "No foot specification exists for the combination of {:?} and {:?}",
                    species, body_type
                );
                return load_mesh("not_found", Vec3::new(-5.0, -5.0, -2.5));
            },
        };
        let lateral = graceful_load_segment(&spec.foot_r.lateral.0);

        generate_mesh(&lateral, Vec3::from(spec.foot_r.offset))
    }
}
////
pub fn mesh_object(obj: object::Body) -> Mesh<FigurePipeline> {
    use object::Body;

    let (name, offset) = match obj {
        Body::Arrow => ("weapon.projectile.simple-arrow", Vec3::new(-5.5, -5.5, 0.0)),
        Body::Bomb => ("object.bomb", Vec3::new(-5.5, -5.5, 0.0)),
        Body::Scarecrow => ("object.scarecrow", Vec3::new(-9.5, -4.0, 0.0)),
        Body::Cauldron => ("object.cauldron", Vec3::new(-10.0, -10.0, 0.0)),
        Body::ChestVines => ("object.chest_vines", Vec3::new(-7.5, -6.0, 0.0)),
        Body::Chest => ("object.chest", Vec3::new(-7.5, -6.0, 0.0)),
        Body::ChestDark => ("object.chest_dark", Vec3::new(-7.5, -6.0, 0.0)),
        Body::ChestDemon => ("object.chest_demon", Vec3::new(-7.5, -6.0, 0.0)),
        Body::ChestGold => ("object.chest_gold", Vec3::new(-7.5, -6.0, 0.0)),
        Body::ChestLight => ("object.chest_light", Vec3::new(-7.5, -6.0, 0.0)),
        Body::ChestOpen => ("object.chest_open", Vec3::new(-7.5, -6.0, 0.0)),
        Body::ChestSkull => ("object.chest_skull", Vec3::new(-7.5, -6.0, 0.0)),
        Body::Pumpkin => ("object.pumpkin", Vec3::new(-5.5, -4.0, 0.0)),
        Body::Pumpkin2 => ("object.pumpkin_2", Vec3::new(-5.0, -4.0, 0.0)),
        Body::Pumpkin3 => ("object.pumpkin_3", Vec3::new(-5.0, -4.0, 0.0)),
        Body::Pumpkin4 => ("object.pumpkin_4", Vec3::new(-5.0, -4.0, 0.0)),
        Body::Pumpkin5 => ("object.pumpkin_5", Vec3::new(-4.0, -5.0, 0.0)),
        Body::Campfire => ("object.campfire", Vec3::new(-9.0, -10.0, 0.0)),
        Body::CampfireLit => ("object.campfire_lit", Vec3::new(-9.0, -10.0, 0.0)),
        Body::LanternGround => ("object.lantern_ground", Vec3::new(-3.5, -3.5, 0.0)),
        Body::LanternGroundOpen => ("object.lantern_ground_open", Vec3::new(-3.5, -3.5, 0.0)),
        Body::LanternStanding => ("object.lantern_standing", Vec3::new(-7.5, -3.5, 0.0)),
        Body::LanternStanding2 => ("object.lantern_standing_2", Vec3::new(-11.5, -3.5, 0.0)),
        Body::PotionRed => ("object.potion_red", Vec3::new(-2.0, -2.0, 0.0)),
        Body::PotionBlue => ("object.potion_blue", Vec3::new(-2.0, -2.0, 0.0)),
        Body::PotionGreen => ("object.potion_green", Vec3::new(-2.0, -2.0, 0.0)),
        Body::Crate => ("object.crate", Vec3::new(-7.0, -7.0, 0.0)),
        Body::Tent => ("object.tent", Vec3::new(-18.5, -19.5, 0.0)),
        Body::WindowSpooky => ("object.window_spooky", Vec3::new(-15.0, -1.5, -1.0)),
        Body::DoorSpooky => ("object.door_spooky", Vec3::new(-15.0, -4.5, 0.0)),
        Body::Table => ("object.table", Vec3::new(-12.0, -8.0, 0.0)),
        Body::Table2 => ("object.table_2", Vec3::new(-8.0, -8.0, 0.0)),
        Body::Table3 => ("object.table_3", Vec3::new(-10.0, -10.0, 0.0)),
        Body::Drawer => ("object.drawer", Vec3::new(-11.0, -7.5, 0.0)),
        Body::BedBlue => ("object.bed_human_blue", Vec3::new(-11.0, -15.0, 0.0)),
        Body::Anvil => ("object.anvil", Vec3::new(-3.0, -7.0, 0.0)),
        Body::Gravestone => ("object.gravestone", Vec3::new(-5.0, -2.0, 0.0)),
        Body::Gravestone2 => ("object.gravestone_2", Vec3::new(-8.5, -3.0, 0.0)),
        Body::Chair => ("object.chair", Vec3::new(-5.0, -4.5, 0.0)),
        Body::Chair2 => ("object.chair_2", Vec3::new(-5.0, -4.5, 0.0)),
        Body::Chair3 => ("object.chair_3", Vec3::new(-5.0, -4.5, 0.0)),
        Body::Bench => ("object.bench", Vec3::new(-8.8, -5.0, 0.0)),
        Body::Carpet => ("object.carpet", Vec3::new(-14.0, -14.0, -0.5)),
        Body::Bedroll => ("object.bedroll", Vec3::new(-11.0, -19.5, -0.5)),
        Body::CarpetHumanRound => ("object.carpet_human_round", Vec3::new(-14.0, -14.0, -0.5)),
        Body::CarpetHumanSquare => ("object.carpet_human_square", Vec3::new(-13.5, -14.0, -0.5)),
        Body::CarpetHumanSquare2 => (
            "object.carpet_human_square_2",
            Vec3::new(-13.5, -14.0, -0.5),
        ),
        Body::CarpetHumanSquircle => (
            "object.carpet_human_squircle",
            Vec3::new(-21.0, -21.0, -0.5),
        ),
        Body::Pouch => ("object.pouch", Vec3::new(-5.5, -4.5, 0.0)),
        Body::CraftingBench => ("object.crafting_bench", Vec3::new(-9.5, -7.0, 0.0)),
        Body::ArrowSnake => ("weapon.projectile.snake-arrow", Vec3::new(-1.5, -6.5, 0.0)),
        Body::BoltFire => ("weapon.projectile.fire-bolt-0", Vec3::new(-3.0, -5.5, -3.0)),
        Body::BoltFireBig => ("weapon.projectile.fire-bolt-1", Vec3::new(-6.0, -6.0, -6.0)),
    };
    load_mesh(name, offset)
}
