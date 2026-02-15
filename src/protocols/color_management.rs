//! wp-color-management-v1 protocol implementation.
//!
//! Provides color management capabilities to Wayland clients, allowing them to describe
//! the color space of their surfaces and query output color properties.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};

use super::raw::wp_color_management::v1::server::{
    wp_color_management_output_v1, wp_color_management_surface_feedback_v1,
    wp_color_management_surface_v1, wp_color_manager_v1, wp_image_description_creator_icc_v1,
    wp_image_description_creator_params_v1, wp_image_description_info_v1,
    wp_image_description_reference_v1, wp_image_description_v1,
};
use wp_color_management_output_v1::WpColorManagementOutputV1;
use wp_color_management_surface_feedback_v1::WpColorManagementSurfaceFeedbackV1;
use wp_color_management_surface_v1::WpColorManagementSurfaceV1;
use wp_color_manager_v1::WpColorManagerV1;
use wp_image_description_creator_icc_v1::WpImageDescriptionCreatorIccV1;
use wp_image_description_creator_params_v1::WpImageDescriptionCreatorParamsV1;
use wp_image_description_info_v1::WpImageDescriptionInfoV1;
use wp_image_description_reference_v1::WpImageDescriptionReferenceV1;
use wp_image_description_v1::WpImageDescriptionV1;

const VERSION: u32 = 1;

static NEXT_IMAGE_DESC_ID: AtomicU32 = AtomicU32::new(1);

fn next_image_desc_id() -> u32 {
    NEXT_IMAGE_DESC_ID.fetch_add(1, Ordering::Relaxed)
}

// --- Image description types ---

/// Describes a color space / image description.
#[derive(Debug, Clone, PartialEq)]
pub enum ImageDescription {
    Srgb,
    Icc {
        data: Vec<u8>,
    },
    Parametric {
        primaries: Option<Primaries>,
        tf: Option<TransferFunction>,
        luminance: Option<LuminanceRange>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Primaries {
    pub r_x: f64,
    pub r_y: f64,
    pub g_x: f64,
    pub g_y: f64,
    pub b_x: f64,
    pub b_y: f64,
    pub w_x: f64,
    pub w_y: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TransferFunction {
    Srgb,
    Linear,
    Gamma(f64),
    Pq,
    Hlg,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LuminanceRange {
    pub min: f64,
    pub max: f64,
    pub reference: f64,
}

/// Per-surface color description.
#[derive(Debug, Clone, Default)]
pub struct SurfaceColorDescription {
    pub description: ImageDescription,
    pub render_intent: RenderIntent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RenderIntent {
    #[default]
    Perceptual,
    Relative,
}

impl Default for ImageDescription {
    fn default() -> Self {
        Self::Srgb
    }
}

// --- Protocol state ---

struct FeedbackSurfaceEntry {
    surface: WlSurface,
    resource: WpColorManagementSurfaceFeedbackV1,
    last_preferred: Option<ImageDescription>,
}

pub struct ColorManagementState {
    #[allow(dead_code)]
    display: DisplayHandle,
    /// Image descriptions keyed by internal ID.
    image_descriptions: HashMap<u32, ImageDescription>,
    /// The sRGB image description ID.
    srgb_id: u32,
    /// Active feedback surfaces for preferred_changed tracking.
    feedback_surfaces: Vec<FeedbackSurfaceEntry>,
}

pub struct ColorManagementGlobalData {
    filter: Box<dyn for<'c> Fn(&'c Client) -> bool + Send + Sync>,
}

// --- Per-object data ---

pub struct ColorManagerData;

pub struct ColorManagementOutputData {
    output: Output,
}

pub struct ColorManagementSurfaceData {
    surface: WlSurface,
}

pub struct ColorManagementSurfaceFeedbackData {
    #[allow(dead_code)]
    surface: WlSurface,
}

pub struct ImageDescriptionData {
    id: u32,
}

pub struct ImageDescriptionReferenceData {
    #[allow(dead_code)]
    id: u32,
}

pub struct ImageDescriptionInfoData {
    #[allow(dead_code)]
    id: u32,
}

/// Builder state for ICC image description creation.
pub struct IccCreatorData;

/// Builder state for parametric image description creation.
pub struct ParametricCreatorData {
    tf: Mutex<Option<TransferFunction>>,
    primaries: Mutex<Option<Primaries>>,
    luminance: Mutex<Option<LuminanceRange>>,
}

// --- Handler trait ---

pub trait ColorManagementHandler {
    fn color_management_state(&mut self) -> &mut ColorManagementState;
    fn get_output_color_description(&self, output: &Output) -> ImageDescription;
    fn get_surface_preferred_description(&self, surface: &WlSurface) -> ImageDescription;
    fn surface_color_changed(&mut self, surface: &WlSurface, desc: &SurfaceColorDescription);
}

// --- State implementation ---

impl ColorManagementState {
    pub fn new<D, F>(display: &DisplayHandle, filter: F) -> Self
    where
        D: GlobalDispatch<WpColorManagerV1, ColorManagementGlobalData>,
        D: Dispatch<WpColorManagerV1, ColorManagerData>,
        D: Dispatch<WpColorManagementOutputV1, ColorManagementOutputData>,
        D: Dispatch<WpColorManagementSurfaceV1, ColorManagementSurfaceData>,
        D: Dispatch<WpColorManagementSurfaceFeedbackV1, ColorManagementSurfaceFeedbackData>,
        D: Dispatch<WpImageDescriptionV1, ImageDescriptionData>,
        D: Dispatch<WpImageDescriptionReferenceV1, ImageDescriptionReferenceData>,
        D: Dispatch<WpImageDescriptionInfoV1, ImageDescriptionInfoData>,
        D: Dispatch<WpImageDescriptionCreatorIccV1, IccCreatorData>,
        D: Dispatch<WpImageDescriptionCreatorParamsV1, ParametricCreatorData>,
        D: ColorManagementHandler,
        D: 'static,
        F: for<'c> Fn(&'c Client) -> bool + Send + Sync + 'static,
    {
        let global_data = ColorManagementGlobalData {
            filter: Box::new(filter),
        };
        display.create_global::<D, WpColorManagerV1, _>(VERSION, global_data);

        let srgb_id = next_image_desc_id();
        let mut image_descriptions = HashMap::new();
        image_descriptions.insert(srgb_id, ImageDescription::Srgb);

        Self {
            display: display.clone(),
            image_descriptions,
            srgb_id,
            feedback_surfaces: Vec::new(),
        }
    }

    /// Create state without registering the Wayland global (for debugging).
    pub fn new_disabled(display: &DisplayHandle) -> Self {
        let srgb_id = next_image_desc_id();
        let mut image_descriptions = HashMap::new();
        image_descriptions.insert(srgb_id, ImageDescription::Srgb);

        Self {
            display: display.clone(),
            image_descriptions,
            srgb_id,
            feedback_surfaces: Vec::new(),
        }
    }

    fn register_description(&mut self, desc: ImageDescription) -> u32 {
        let id = next_image_desc_id();
        self.image_descriptions.insert(id, desc);
        id
    }

    /// Check all tracked feedback surfaces and send `preferred_changed` if the
    /// preferred description for any surface has changed (e.g. because the surface
    /// moved to a different output).
    pub fn notify_preferred_changed(
        &mut self,
        get_preferred: impl Fn(&WlSurface) -> ImageDescription,
    ) {
        // Remove entries whose protocol resource has been destroyed.
        self.feedback_surfaces
            .retain(|e| e.resource.is_alive() && e.surface.is_alive());

        for entry in &mut self.feedback_surfaces {
            let desc = get_preferred(&entry.surface);

            let changed = match &entry.last_preferred {
                Some(prev) => *prev != desc,
                None => true,
            };

            if changed {
                entry.last_preferred = Some(desc.clone());
                let id = next_image_desc_id();
                self.image_descriptions.insert(id, desc);
                entry.resource.preferred_changed(id);
            }
        }
    }
}

// --- GlobalDispatch for the manager ---

impl<D> GlobalDispatch<WpColorManagerV1, ColorManagementGlobalData, D> for ColorManagementState
where
    D: GlobalDispatch<WpColorManagerV1, ColorManagementGlobalData>,
    D: Dispatch<WpColorManagerV1, ColorManagerData>,
    D: ColorManagementHandler,
    D: 'static,
{
    fn bind(
        _state: &mut D,
        _handle: &DisplayHandle,
        _client: &Client,
        manager: New<WpColorManagerV1>,
        _manager_state: &ColorManagementGlobalData,
        data_init: &mut DataInit<'_, D>,
    ) {
        let resource = data_init.init(manager, ColorManagerData);

        // Send supported features.
        resource.supported_feature(wp_color_manager_v1::Feature::IccV2V4);
        resource.supported_feature(wp_color_manager_v1::Feature::Parametric);
        resource.supported_feature(wp_color_manager_v1::Feature::SetPrimaries);
        resource.supported_feature(wp_color_manager_v1::Feature::SetTfPower);
        resource.supported_feature(wp_color_manager_v1::Feature::SetLuminances);

        // Send supported intents.
        resource.supported_intent(wp_color_manager_v1::RenderIntent::Perceptual);
        resource.supported_intent(wp_color_manager_v1::RenderIntent::Relative);

        // Send supported transfer functions.
        resource.supported_tf_named(wp_color_manager_v1::TransferFunction::Srgb);
        resource.supported_tf_named(wp_color_manager_v1::TransferFunction::ExtLinear);
        resource.supported_tf_named(wp_color_manager_v1::TransferFunction::St2084Pq);
        resource.supported_tf_named(wp_color_manager_v1::TransferFunction::Hlg);

        // Send supported primaries.
        resource.supported_primaries_named(wp_color_manager_v1::Primaries::Srgb);
        resource.supported_primaries_named(wp_color_manager_v1::Primaries::Bt2020);
        resource.supported_primaries_named(wp_color_manager_v1::Primaries::DisplayP3);

        // Signal that all feature advertisements are done.
        resource.done();
    }

    fn can_view(client: Client, global_data: &ColorManagementGlobalData) -> bool {
        (global_data.filter)(&client)
    }
}

// --- Dispatch for WpColorManagerV1 ---

impl<D> Dispatch<WpColorManagerV1, ColorManagerData, D> for ColorManagementState
where
    D: Dispatch<WpColorManagerV1, ColorManagerData>,
    D: Dispatch<WpColorManagementOutputV1, ColorManagementOutputData>,
    D: Dispatch<WpColorManagementSurfaceV1, ColorManagementSurfaceData>,
    D: Dispatch<WpColorManagementSurfaceFeedbackV1, ColorManagementSurfaceFeedbackData>,
    D: Dispatch<WpImageDescriptionV1, ImageDescriptionData>,
    D: Dispatch<WpImageDescriptionReferenceV1, ImageDescriptionReferenceData>,
    D: Dispatch<WpImageDescriptionCreatorIccV1, IccCreatorData>,
    D: Dispatch<WpImageDescriptionCreatorParamsV1, ParametricCreatorData>,
    D: ColorManagementHandler,
    D: 'static,
{
    fn request(
        state: &mut D,
        _client: &Client,
        _resource: &WpColorManagerV1,
        request: <WpColorManagerV1 as Resource>::Request,
        _data: &ColorManagerData,
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            wp_color_manager_v1::Request::GetOutput { id, output } => {
                if let Some(output) = Output::from_resource(&output) {
                    data_init.init(id, ColorManagementOutputData { output });
                } else {
                    data_init.init(
                        id,
                        ColorManagementOutputData {
                            output: Output::new(
                                "unknown".into(),
                                smithay::output::PhysicalProperties {
                                    size: (0, 0).into(),
                                    subpixel: smithay::output::Subpixel::Unknown,
                                    make: "Unknown".into(),
                                    model: "Unknown".into(),
                                    serial_number: "Unknown".into(),
                                },
                            ),
                        },
                    );
                }
            }
            wp_color_manager_v1::Request::GetSurface { id, surface } => {
                data_init.init(
                    id,
                    ColorManagementSurfaceData {
                        surface: surface.clone(),
                    },
                );
            }
            wp_color_manager_v1::Request::GetSurfaceFeedback { id, surface } => {
                let resource = data_init.init(
                    id,
                    ColorManagementSurfaceFeedbackData {
                        surface: surface.clone(),
                    },
                );
                state
                    .color_management_state()
                    .feedback_surfaces
                    .push(FeedbackSurfaceEntry {
                        surface: surface.clone(),
                        resource,
                        last_preferred: None,
                    });
            }
            wp_color_manager_v1::Request::CreateIccCreator { obj } => {
                data_init.init(obj, IccCreatorData);
            }
            wp_color_manager_v1::Request::CreateParametricCreator { obj } => {
                data_init.init(
                    obj,
                    ParametricCreatorData {
                        tf: Mutex::new(None),
                        primaries: Mutex::new(None),
                        luminance: Mutex::new(None),
                    },
                );
            }
            wp_color_manager_v1::Request::CreateWindowsScrgb { image_description } => {
                // Not fully supported yet; create as sRGB.
                let srgb_id = state.color_management_state().srgb_id;
                let desc =
                    data_init.init(image_description, ImageDescriptionData { id: srgb_id });
                desc.ready(srgb_id);
            }
            wp_color_manager_v1::Request::GetImageDescription {
                image_description,
                reference,
            } => {
                let ref_data: &ImageDescriptionReferenceData = reference.data().unwrap();
                let id = ref_data.id;
                let desc = data_init.init(image_description, ImageDescriptionData { id });
                desc.ready(id);
            }
            wp_color_manager_v1::Request::Destroy => (),
        }
    }
}

// --- Dispatch for WpColorManagementOutputV1 ---

impl<D> Dispatch<WpColorManagementOutputV1, ColorManagementOutputData, D> for ColorManagementState
where
    D: Dispatch<WpColorManagementOutputV1, ColorManagementOutputData>,
    D: Dispatch<WpImageDescriptionV1, ImageDescriptionData>,
    D: ColorManagementHandler,
    D: 'static,
{
    fn request(
        state: &mut D,
        _client: &Client,
        _resource: &WpColorManagementOutputV1,
        request: <WpColorManagementOutputV1 as Resource>::Request,
        data: &ColorManagementOutputData,
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            wp_color_management_output_v1::Request::GetImageDescription { image_description } => {
                let desc = state.get_output_color_description(&data.output);
                let id = state.color_management_state().register_description(desc);
                let img_desc = data_init.init(image_description, ImageDescriptionData { id });
                img_desc.ready(id);
            }
            wp_color_management_output_v1::Request::Destroy => (),
        }
    }
}

// --- Dispatch for WpColorManagementSurfaceV1 ---

impl<D> Dispatch<WpColorManagementSurfaceV1, ColorManagementSurfaceData, D>
    for ColorManagementState
where
    D: Dispatch<WpColorManagementSurfaceV1, ColorManagementSurfaceData>,
    D: ColorManagementHandler,
    D: 'static,
{
    fn request(
        state: &mut D,
        _client: &Client,
        _resource: &WpColorManagementSurfaceV1,
        request: <WpColorManagementSurfaceV1 as Resource>::Request,
        data: &ColorManagementSurfaceData,
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            wp_color_management_surface_v1::Request::SetImageDescription {
                image_description,
                render_intent,
            } => {
                let cm_state = state.color_management_state();
                let img_data: &ImageDescriptionData = image_description.data().unwrap();
                let desc = cm_state
                    .image_descriptions
                    .get(&img_data.id)
                    .cloned()
                    .unwrap_or_default();
                use smithay::reexports::wayland_server::backend::protocol::WEnum;
                let intent = match render_intent {
                    WEnum::Value(wp_color_manager_v1::RenderIntent::Relative) => {
                        RenderIntent::Relative
                    }
                    _ => RenderIntent::Perceptual,
                };
                let surface_desc = SurfaceColorDescription {
                    description: desc,
                    render_intent: intent,
                };
                state.surface_color_changed(&data.surface, &surface_desc);
            }
            wp_color_management_surface_v1::Request::UnsetImageDescription => {
                let surface_desc = SurfaceColorDescription {
                    description: ImageDescription::Srgb,
                    render_intent: RenderIntent::Perceptual,
                };
                state.surface_color_changed(&data.surface, &surface_desc);
            }
            wp_color_management_surface_v1::Request::Destroy => (),
        }
    }
}

// --- Dispatch for WpColorManagementSurfaceFeedbackV1 ---

impl<D> Dispatch<WpColorManagementSurfaceFeedbackV1, ColorManagementSurfaceFeedbackData, D>
    for ColorManagementState
where
    D: Dispatch<WpColorManagementSurfaceFeedbackV1, ColorManagementSurfaceFeedbackData>,
    D: Dispatch<WpImageDescriptionV1, ImageDescriptionData>,
    D: ColorManagementHandler,
    D: 'static,
{
    fn request(
        state: &mut D,
        _client: &Client,
        _resource: &WpColorManagementSurfaceFeedbackV1,
        request: <WpColorManagementSurfaceFeedbackV1 as Resource>::Request,
        _data: &ColorManagementSurfaceFeedbackData,
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            wp_color_management_surface_feedback_v1::Request::GetPreferred {
                image_description,
            } => {
                let desc = state.get_surface_preferred_description(&_data.surface);
                let id = state.color_management_state().register_description(desc);
                let img_desc = data_init.init(image_description, ImageDescriptionData { id });
                img_desc.ready(id);
            }
            wp_color_management_surface_feedback_v1::Request::GetPreferredParametric {
                image_description,
            } => {
                let desc = state.get_surface_preferred_description(&_data.surface);
                let id = state.color_management_state().register_description(desc);
                let img_desc = data_init.init(image_description, ImageDescriptionData { id });
                img_desc.ready(id);
            }
            wp_color_management_surface_feedback_v1::Request::Destroy => {
                state
                    .color_management_state()
                    .feedback_surfaces
                    .retain(|e| e.resource != *_resource);
            }
        }
    }
}

// --- Dispatch for WpImageDescriptionV1 ---

impl<D> Dispatch<WpImageDescriptionV1, ImageDescriptionData, D> for ColorManagementState
where
    D: Dispatch<WpImageDescriptionV1, ImageDescriptionData>,
    D: Dispatch<WpImageDescriptionInfoV1, ImageDescriptionInfoData>,
    D: Dispatch<WpImageDescriptionReferenceV1, ImageDescriptionReferenceData>,
    D: ColorManagementHandler,
    D: 'static,
{
    fn request(
        state: &mut D,
        _client: &Client,
        _resource: &WpImageDescriptionV1,
        request: <WpImageDescriptionV1 as Resource>::Request,
        data: &ImageDescriptionData,
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            wp_image_description_v1::Request::GetInformation { information } => {
                let info_resource =
                    data_init.init(information, ImageDescriptionInfoData { id: data.id });

                let cm_state = state.color_management_state();
                let desc = cm_state
                    .image_descriptions
                    .get(&data.id)
                    .cloned()
                    .unwrap_or_default();

                send_image_description_info(&info_resource, &desc);
                info_resource.done();
            }
            wp_image_description_v1::Request::Destroy => (),
        }
    }
}

// --- Dispatch for WpImageDescriptionReferenceV1 ---

impl<D> Dispatch<WpImageDescriptionReferenceV1, ImageDescriptionReferenceData, D>
    for ColorManagementState
where
    D: Dispatch<WpImageDescriptionReferenceV1, ImageDescriptionReferenceData>,
    D: 'static,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        _resource: &WpImageDescriptionReferenceV1,
        request: <WpImageDescriptionReferenceV1 as Resource>::Request,
        _data: &ImageDescriptionReferenceData,
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            wp_image_description_reference_v1::Request::Destroy => (),
        }
    }
}

// --- Dispatch for WpImageDescriptionInfoV1 ---

impl<D> Dispatch<WpImageDescriptionInfoV1, ImageDescriptionInfoData, D> for ColorManagementState
where
    D: Dispatch<WpImageDescriptionInfoV1, ImageDescriptionInfoData>,
    D: 'static,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        _resource: &WpImageDescriptionInfoV1,
        request: <WpImageDescriptionInfoV1 as Resource>::Request,
        _data: &ImageDescriptionInfoData,
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        let _ = request;
    }
}

// --- Dispatch for WpImageDescriptionCreatorIccV1 ---

impl<D> Dispatch<WpImageDescriptionCreatorIccV1, IccCreatorData, D> for ColorManagementState
where
    D: Dispatch<WpImageDescriptionCreatorIccV1, IccCreatorData>,
    D: Dispatch<WpImageDescriptionV1, ImageDescriptionData>,
    D: ColorManagementHandler,
    D: 'static,
{
    fn request(
        state: &mut D,
        _client: &Client,
        _resource: &WpImageDescriptionCreatorIccV1,
        request: <WpImageDescriptionCreatorIccV1 as Resource>::Request,
        _data: &IccCreatorData,
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            wp_image_description_creator_icc_v1::Request::Create { image_description } => {
                // For now, create a basic sRGB description.
                // A full implementation would parse the ICC data set via SetIccFile.
                let srgb_id = state.color_management_state().srgb_id;
                let desc =
                    data_init.init(image_description, ImageDescriptionData { id: srgb_id });
                desc.ready(srgb_id);
            }
            wp_image_description_creator_icc_v1::Request::SetIccFile { .. } => {
                // ICC file data is noted but not yet processed.
                // A full implementation would store via interior mutability.
            }
        }
    }
}

// --- Dispatch for WpImageDescriptionCreatorParamsV1 ---

impl<D> Dispatch<WpImageDescriptionCreatorParamsV1, ParametricCreatorData, D>
    for ColorManagementState
where
    D: Dispatch<WpImageDescriptionCreatorParamsV1, ParametricCreatorData>,
    D: Dispatch<WpImageDescriptionV1, ImageDescriptionData>,
    D: ColorManagementHandler,
    D: 'static,
{
    fn request(
        state: &mut D,
        _client: &Client,
        _resource: &WpImageDescriptionCreatorParamsV1,
        request: <WpImageDescriptionCreatorParamsV1 as Resource>::Request,
        data: &ParametricCreatorData,
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            wp_image_description_creator_params_v1::Request::Create { image_description } => {
                let tf = data.tf.lock().unwrap().clone();
                let primaries = data.primaries.lock().unwrap().clone();
                let luminance = data.luminance.lock().unwrap().clone();

                let desc_value = ImageDescription::Parametric {
                    primaries,
                    tf,
                    luminance,
                };
                let id = state
                    .color_management_state()
                    .register_description(desc_value);
                let desc = data_init.init(image_description, ImageDescriptionData { id });
                desc.ready(id);
            }
            wp_image_description_creator_params_v1::Request::SetTfNamed { tf } => {
                use smithay::reexports::wayland_server::backend::protocol::WEnum;
                let transfer = match tf {
                    WEnum::Value(wp_color_manager_v1::TransferFunction::Srgb) => {
                        TransferFunction::Srgb
                    }
                    WEnum::Value(wp_color_manager_v1::TransferFunction::ExtLinear) => {
                        TransferFunction::Linear
                    }
                    WEnum::Value(wp_color_manager_v1::TransferFunction::St2084Pq) => {
                        TransferFunction::Pq
                    }
                    WEnum::Value(wp_color_manager_v1::TransferFunction::Hlg) => {
                        TransferFunction::Hlg
                    }
                    _ => TransferFunction::Srgb,
                };
                *data.tf.lock().unwrap() = Some(transfer);
            }
            wp_image_description_creator_params_v1::Request::SetTfPower { eexp } => {
                let gamma = eexp as f64 / 10000.0;
                *data.tf.lock().unwrap() = Some(TransferFunction::Gamma(gamma));
            }
            wp_image_description_creator_params_v1::Request::SetPrimariesNamed { primaries } => {
                use smithay::reexports::wayland_server::backend::protocol::WEnum;
                let p = match primaries {
                    WEnum::Value(wp_color_manager_v1::Primaries::Srgb) => Primaries {
                        r_x: 0.64,
                        r_y: 0.33,
                        g_x: 0.30,
                        g_y: 0.60,
                        b_x: 0.15,
                        b_y: 0.06,
                        w_x: 0.3127,
                        w_y: 0.3290,
                    },
                    WEnum::Value(wp_color_manager_v1::Primaries::Bt2020) => Primaries {
                        r_x: 0.708,
                        r_y: 0.292,
                        g_x: 0.170,
                        g_y: 0.797,
                        b_x: 0.131,
                        b_y: 0.046,
                        w_x: 0.3127,
                        w_y: 0.3290,
                    },
                    WEnum::Value(wp_color_manager_v1::Primaries::DisplayP3) => Primaries {
                        r_x: 0.680,
                        r_y: 0.320,
                        g_x: 0.265,
                        g_y: 0.690,
                        b_x: 0.150,
                        b_y: 0.060,
                        w_x: 0.3127,
                        w_y: 0.3290,
                    },
                    _ => Primaries {
                        r_x: 0.64,
                        r_y: 0.33,
                        g_x: 0.30,
                        g_y: 0.60,
                        b_x: 0.15,
                        b_y: 0.06,
                        w_x: 0.3127,
                        w_y: 0.3290,
                    },
                };
                *data.primaries.lock().unwrap() = Some(p);
            }
            wp_image_description_creator_params_v1::Request::SetPrimaries {
                r_x,
                r_y,
                g_x,
                g_y,
                b_x,
                b_y,
                w_x,
                w_y,
            } => {
                // Protocol sends CIE xy × 1,000,000.
                *data.primaries.lock().unwrap() = Some(Primaries {
                    r_x: r_x as f64 / 1000000.0,
                    r_y: r_y as f64 / 1000000.0,
                    g_x: g_x as f64 / 1000000.0,
                    g_y: g_y as f64 / 1000000.0,
                    b_x: b_x as f64 / 1000000.0,
                    b_y: b_y as f64 / 1000000.0,
                    w_x: w_x as f64 / 1000000.0,
                    w_y: w_y as f64 / 1000000.0,
                });
            }
            wp_image_description_creator_params_v1::Request::SetLuminances {
                min_lum,
                max_lum,
                reference_lum,
            } => {
                *data.luminance.lock().unwrap() = Some(LuminanceRange {
                    min: min_lum as f64 / 10000.0,
                    max: max_lum as f64,
                    reference: reference_lum as f64,
                });
            }
            _ => (),
        }
    }
}

// --- Helper functions ---

fn send_image_description_info(info: &WpImageDescriptionInfoV1, desc: &ImageDescription) {
    match desc {
        ImageDescription::Srgb => {
            info.tf_named(wp_color_manager_v1::TransferFunction::Srgb);
            info.primaries_named(wp_color_manager_v1::Primaries::Srgb);
            // sRGB primaries: R(0.64,0.33) G(0.30,0.60) B(0.15,0.06) W(0.3127,0.3290)
            // Protocol uses CIE xy × 1,000,000.
            info.primaries(640000, 330000, 300000, 600000, 150000, 60000, 312700, 329000);
            info.luminances(0, 80, 80);
        }
        ImageDescription::Icc { .. } => {
            // For ICC profiles, report sRGB as a fallback for now.
            info.tf_named(wp_color_manager_v1::TransferFunction::Srgb);
            info.primaries_named(wp_color_manager_v1::Primaries::Srgb);
        }
        ImageDescription::Parametric {
            primaries,
            tf,
            luminance,
        } => {
            match tf {
                Some(TransferFunction::Srgb) | None => {
                    info.tf_named(wp_color_manager_v1::TransferFunction::Srgb);
                }
                Some(TransferFunction::Linear) => {
                    info.tf_named(wp_color_manager_v1::TransferFunction::ExtLinear);
                }
                Some(TransferFunction::Gamma(g)) => {
                    info.tf_power((*g * 10000.0) as u32);
                }
                Some(TransferFunction::Pq) => {
                    info.tf_named(wp_color_manager_v1::TransferFunction::St2084Pq);
                }
                Some(TransferFunction::Hlg) => {
                    info.tf_named(wp_color_manager_v1::TransferFunction::Hlg);
                }
            }
            if let Some(p) = primaries {
                // Protocol uses CIE xy × 1,000,000.
                info.primaries(
                    (p.r_x * 1000000.0) as i32,
                    (p.r_y * 1000000.0) as i32,
                    (p.g_x * 1000000.0) as i32,
                    (p.g_y * 1000000.0) as i32,
                    (p.b_x * 1000000.0) as i32,
                    (p.b_y * 1000000.0) as i32,
                    (p.w_x * 1000000.0) as i32,
                    (p.w_y * 1000000.0) as i32,
                );
            }
            if let Some(l) = luminance {
                info.luminances(
                    (l.min * 10000.0) as u32,
                    l.max as u32,
                    l.reference as u32,
                );
            }
        }
    }
}

// --- Delegate macro ---

#[macro_export]
macro_rules! delegate_color_management {
    ($(@<$( $lt:tt $( : $clt:tt $(+ $dlt:tt )* )? ),+>)? $ty: ty) => {
        smithay::reexports::wayland_server::delegate_global_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            $crate::protocols::raw::wp_color_management::v1::server::wp_color_manager_v1::WpColorManagerV1: $crate::protocols::color_management::ColorManagementGlobalData
        ] => $crate::protocols::color_management::ColorManagementState);

        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            $crate::protocols::raw::wp_color_management::v1::server::wp_color_manager_v1::WpColorManagerV1: $crate::protocols::color_management::ColorManagerData
        ] => $crate::protocols::color_management::ColorManagementState);

        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            $crate::protocols::raw::wp_color_management::v1::server::wp_color_management_output_v1::WpColorManagementOutputV1: $crate::protocols::color_management::ColorManagementOutputData
        ] => $crate::protocols::color_management::ColorManagementState);

        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            $crate::protocols::raw::wp_color_management::v1::server::wp_color_management_surface_v1::WpColorManagementSurfaceV1: $crate::protocols::color_management::ColorManagementSurfaceData
        ] => $crate::protocols::color_management::ColorManagementState);

        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            $crate::protocols::raw::wp_color_management::v1::server::wp_color_management_surface_feedback_v1::WpColorManagementSurfaceFeedbackV1: $crate::protocols::color_management::ColorManagementSurfaceFeedbackData
        ] => $crate::protocols::color_management::ColorManagementState);

        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            $crate::protocols::raw::wp_color_management::v1::server::wp_image_description_v1::WpImageDescriptionV1: $crate::protocols::color_management::ImageDescriptionData
        ] => $crate::protocols::color_management::ColorManagementState);

        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            $crate::protocols::raw::wp_color_management::v1::server::wp_image_description_reference_v1::WpImageDescriptionReferenceV1: $crate::protocols::color_management::ImageDescriptionReferenceData
        ] => $crate::protocols::color_management::ColorManagementState);

        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            $crate::protocols::raw::wp_color_management::v1::server::wp_image_description_info_v1::WpImageDescriptionInfoV1: $crate::protocols::color_management::ImageDescriptionInfoData
        ] => $crate::protocols::color_management::ColorManagementState);

        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            $crate::protocols::raw::wp_color_management::v1::server::wp_image_description_creator_icc_v1::WpImageDescriptionCreatorIccV1: $crate::protocols::color_management::IccCreatorData
        ] => $crate::protocols::color_management::ColorManagementState);

        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            $crate::protocols::raw::wp_color_management::v1::server::wp_image_description_creator_params_v1::WpImageDescriptionCreatorParamsV1: $crate::protocols::color_management::ParametricCreatorData
        ] => $crate::protocols::color_management::ColorManagementState);
    };
}
