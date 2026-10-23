//! Capability manifests: one JSON file per model (or published tool).
//!
//! A manifest is pure data. It lists the model files, a memory profile, the
//! operations the model supports (each backed by a ComfyUI workflow template),
//! and the inputs the UI should render. Inputs are drawn from a fixed catalogue
//! of [`InputKind`]s, so adding a model never needs UI code.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub const MANIFEST_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Manifest {
    pub schema: u32,
    pub id: String,
    pub display_name: String,
    /// Model family, used for grouping and model-file matching (e.g. "sdxl", "flux1").
    pub family: String,
    pub media: Media,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub license: License,
    /// Files the model needs, by role. Roles are referenced from templates as `{{file.<role>}}`.
    #[serde(default)]
    pub files: Vec<ModelFile>,
    #[serde(default)]
    pub memory: MemoryProfile,
    /// Operation name -> how to run it.
    pub ops: BTreeMap<String, OpSpec>,
    pub inputs: Vec<InputSpec>,
    #[serde(default)]
    pub presets: Vec<Preset>,
    /// Where this manifest came from. Filled by the loader, not by authors.
    #[serde(default, skip_deserializing)]
    pub origin: Origin,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Media {
    Image,
    Video,
    Audio,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct License {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub commercial: bool,
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelFile {
    /// Role name, e.g. "checkpoint", "diffusion", "text_encoder", "vae", "clip_l".
    pub role: String,
    /// ComfyUI model folder: checkpoints, diffusion_models, text_encoders, vae, loras, ...
    pub folder: String,
    /// Alternative variants of this file. The planner picks one.
    pub variants: Vec<FileVariant>,
    #[serde(default)]
    pub optional: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FileVariant {
    /// File name as ComfyUI will see it inside `folder`.
    pub name: String,
    pub precision: Precision,
    pub size_gb: f64,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub sha256: Option<String>,
}

/// Weight precision. Ordered roughly from highest to lowest quality.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Precision {
    Fp32,
    Bf16,
    Fp16,
    Fp8,
    Q8,
    Nvfp4,
    Q6,
    Q5,
    Q4,
    Q3,
}

impl Precision {
    /// Relative quality score, 100 = full precision.
    pub fn quality(self) -> u32 {
        match self {
            Precision::Fp32 | Precision::Bf16 | Precision::Fp16 => 100,
            Precision::Q8 => 98,
            Precision::Fp8 => 97,
            Precision::Nvfp4 => 94,
            Precision::Q6 => 93,
            Precision::Q5 => 90,
            Precision::Q4 => 85,
            Precision::Q3 => 75,
        }
    }

    pub fn is_gguf(self) -> bool {
        matches!(
            self,
            Precision::Q8 | Precision::Q6 | Precision::Q5 | Precision::Q4 | Precision::Q3
        )
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MemoryProfile {
    /// Extra GPU memory needed per megapixel of output (per frame for video), in GB.
    #[serde(default = "default_activation")]
    pub activation_gb_per_mp: f64,
    /// For video: the activation cost grows with frames. GB per megapixel per frame.
    #[serde(default)]
    pub activation_gb_per_mp_frame: f64,
    /// Peak VAE decode memory per megapixel (per frame for video), in GB.
    #[serde(default = "default_vae")]
    pub vae_gb_per_mp: f64,
}

fn default_activation() -> f64 {
    1.0
}
fn default_vae() -> f64 {
    1.5
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OpSpec {
    /// Workflow template path, relative to the manifests root, or inline graph.
    pub workflow: WorkflowRef,
    /// Graph expanders applied before placeholder substitution.
    #[serde(default)]
    pub expand: Vec<Expander>,
    /// Inputs that must be present for this op (beyond those marked required).
    #[serde(default)]
    pub requires: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum WorkflowRef {
    Path(String),
    Inline(Value),
}

/// Repeats a fragment of nodes once per item of a list input, threading links through it.
///
/// `chain` lists links (`[node_id, slot]`) in the base workflow. The fragment's `head`
/// node outputs replace them: item 1 consumes the original links via `{{prev:N}}`,
/// item 2 consumes item 1's head, and after expansion every other consumer of
/// `chain[N]` is rewired to the last head's slot N. With an empty list nothing changes.
/// This one mechanism covers LoRA stacks, regional prompts, reference-image chains, etc.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Expander {
    /// List input id to iterate.
    pub input: String,
    pub chain: Vec<Value>,
    /// Name of the node in `fragment` whose outputs continue the chain.
    pub head: String,
    /// Node name -> ComfyUI node (`class_type`, `inputs`). References to sibling nodes use `["@name", slot]`.
    pub fragment: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InputSpec {
    pub id: String,
    pub kind: InputKind,
    pub label: String,
    #[serde(default)]
    pub help: String,
    #[serde(default)]
    pub level: Level,
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub default: Option<Value>,
    #[serde(default)]
    pub required: bool,
    /// Only shown and bound for these ops. Empty = all ops.
    #[serde(default)]
    pub ops: Vec<String>,
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
    #[serde(default)]
    pub step: Option<f64>,
    /// Choices for `enum`, or size presets for `size`.
    #[serde(default)]
    pub options: Vec<Value>,
    /// JSON Schema for `structured_prompt` values; drives the generated form.
    #[serde(default)]
    pub schema: Option<Value>,
    /// Extra kind-specific settings (e.g. `multiple_of` for size, `max_items` for lists).
    #[serde(default)]
    pub extra: BTreeMap<String, Value>,
}

/// The fixed catalogue of input mechanisms. Each has one generic UI widget.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum InputKind {
    /// Prompt text. Value: string.
    Text,
    /// Negative prompt. Value: string.
    NegativeText,
    /// JSON prompt edited through a schema-driven form or raw JSON. Value: object.
    StructuredPrompt,
    /// Boxes on the canvas. Value: `[Region]`.
    Regions,
    /// Painted mask. Value: asset id (white = edit).
    Mask,
    /// Source image for img2img/edit/inpaint. Value: asset id.
    Image,
    /// Reference images. Value: `[asset id]`.
    ImageList,
    /// First frame for video. Value: asset id.
    FirstFrame,
    /// Last frame for video. Value: asset id.
    LastFrame,
    /// Input video. Value: asset id.
    Video,
    /// Input audio. Value: asset id.
    Audio,
    /// Control image (pose, depth, canny...). Value: asset id.
    ControlImage,
    /// LoRA stack. Value: `[{"file": name, "strength": f}]`.
    LoraStack,
    /// Width and height. Value: `{"w": int, "h": int}`.
    Size,
    /// Seed. Value: int, -1 means random.
    Seed,
    Int,
    Float,
    Bool,
    /// One of `options`. Value: string or number.
    Enum,
    /// Model file picker inside a ComfyUI folder (`extra.folder`). Value: file name.
    ModelFile,
    /// Camera or motion preset; behaves like `enum` with richer presentation.
    Motion,
    /// Free text passed through unchanged (escape hatch for any widget).
    Raw,
}

impl InputKind {
    /// Kinds whose values are uploaded files.
    pub fn is_asset(self) -> bool {
        matches!(
            self,
            InputKind::Mask
                | InputKind::Image
                | InputKind::FirstFrame
                | InputKind::LastFrame
                | InputKind::Video
                | InputKind::Audio
                | InputKind::ControlImage
        )
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    /// Shown in Simple mode.
    #[default]
    Simple,
    /// Shown in the Advanced section.
    Advanced,
    /// Shown only in the Ultra drawer.
    Ultra,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Preset {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub values: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    #[default]
    Builtin,
    User,
    Tool,
}

/// One box drawn on the canvas. Coordinates are fractions of the image (0..1).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Region {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    #[serde(default)]
    pub prompt: String,
    /// "obj" or "text" (text boxes render the given text, for layout-aware models).
    #[serde(default = "default_region_type")]
    pub r#type: String,
    #[serde(default)]
    pub text: String,
    #[serde(default = "default_weight")]
    pub weight: f64,
    #[serde(default)]
    pub palette: Vec<String>,
}

fn default_region_type() -> String {
    "obj".into()
}
fn default_weight() -> f64 {
    1.0
}

impl Manifest {
    pub fn input(&self, id: &str) -> Option<&InputSpec> {
        self.inputs.iter().find(|i| i.id == id)
    }

    /// Inputs that apply to the given op.
    pub fn inputs_for(&self, op: &str) -> impl Iterator<Item = &InputSpec> {
        self.inputs
            .iter()
            .filter(move |i| i.ops.is_empty() || i.ops.iter().any(|o| o == op))
    }

    pub fn total_size_gb(&self) -> f64 {
        self.files
            .iter()
            .filter(|f| !f.optional)
            .filter_map(|f| f.variants.first())
            .map(|v| v.size_gb)
            .sum()
    }
}
