use std::f64::consts::PI;
use std::fs::File;
use std::num::NonZeroUsize;
use std::path::Path;

use anyhow::{Result, anyhow, bail};
use vello::kurbo::{Affine, BezPath, Circle, Point, Rect, Stroke, Triangle};
use vello::peniko::color::HueDirection;
use vello::peniko::color::palette::css;
use vello::peniko::{Color, Fill};
use vello::util::{RenderContext, block_on_wgpu};
use vello::wgpu::wgt::{CommandEncoderDescriptor, TextureDescriptor};
use vello::wgpu::{
    BufferDescriptor, BufferUsages, Extent3d, MapMode, TexelCopyBufferInfo,
    TexelCopyBufferLayout, TextureDimension, TextureFormat, TextureUsages,
};
use vello::{
    AaConfig, AaSupport, RenderParams, Renderer, RendererOptions, Scene,
};

fn main() -> Result<()> {
    env_logger::init();
    pollster::block_on(render())?;
    Ok(())
}

async fn render() -> Result<()> {
    let mut context = RenderContext::new();
    let device_id = context
        .device(None)
        .await
        .ok_or_else(|| anyhow!("no such render context"))?;
    let handle = &mut context.devices[device_id];
    let device = &handle.device;
    let queue = &handle.queue;

    let mut renderer = Renderer::new(
        device,
        RendererOptions {
            num_init_threads: NonZeroUsize::new(1),
            antialiasing_support: AaSupport::area_only(),
            ..Default::default()
        },
    )
    .or_else(|_| bail!("failed to create renderer"))?;
    let (width, height) = (Player::DIMENSION as u32, Player::DIMENSION as u32);

    let scene = Player::create_scene()?;
    let size = Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    };
    let target = device.create_texture(&TextureDescriptor {
        label: Some("Flight texture"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: TextureDimension::D2,
        format: TextureFormat::Rgba8Unorm,
        usage: TextureUsages::STORAGE_BINDING | TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&Default::default());
    renderer
        .render_to_texture(
            device,
            queue,
            &scene,
            &view,
            &RenderParams {
                base_color: css::GRAY.lerp(
                    css::WHITE,
                    0.5,
                    HueDirection::Increasing,
                ),
                width,
                height,
                antialiasing_method: AaConfig::Area,
            },
        )
        .or_else(|_| bail!("Got non-Send/Sync error from rendering"))?;
    let stride = (width * 4).next_multiple_of(256);
    let buffer = device.create_buffer(&BufferDescriptor {
        label: Some("val"),
        size: (stride * height).into(),
        usage: BufferUsages::MAP_READ | BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder =
        device.create_command_encoder(&CommandEncoderDescriptor {
            label: Some("Copy out buffer"),
        });
    encoder.copy_texture_to_buffer(
        target.as_image_copy(),
        TexelCopyBufferInfo {
            buffer: &buffer,
            layout: TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(stride),
                rows_per_image: None,
            },
        },
        size,
    );
    queue.submit([encoder.finish()]);

    let slice = buffer.slice(..);

    let (tx, rx) = futures_intrusive::channel::shared::oneshot_channel();
    slice.map_async(MapMode::Read, move |v| tx.send(v).unwrap());
    block_on_wgpu(device, rx.receive())
        .map(|r| r.map_err(|e| e.into()))
        .unwrap_or_else(|| bail!("channel was closed"))?;

    let data = slice.get_mapped_range();
    let mut bytes = Vec::<u8>::with_capacity((width * height * 4).try_into()?);
    for row in 0..height {
        let start = (row * stride).try_into()?;
        bytes.extend(&data[start..start + (width * 4) as usize]);
    }
    let path = Path::new("background.png");
    let mut file = File::create(path)?;
    let mut encoder = png::Encoder::new(&mut file, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(&bytes)?;
    writer.finish()?;
    Ok(())
}

trait Drawable {
    fn draw(&self, scene: &mut Scene);
}

struct Player {
    color: Color,
    affine: Affine,
}

#[derive(Copy, Clone)]
enum CellKind {
    #[allow(unused)]
    Triangle0,
    Triangle90,
    Triangle180,
    Triangle270,
    VBlock,
    HBlock,
}

pub struct Cell {
    kind: CellKind,
    color: Color,
    affine: Affine,
    origin: Point,
}

impl Cell {
    const DIM: f64 = 128.0;

    const DIM_X2: f64 = Self::DIM * 2.0;

    const DIM_X4: f64 = Self::DIM * 4.0;

    const RADIUS: f64 = Self::DIM * 0.35;

    fn new(
        kind: CellKind,
        color: Color,
        affine: Affine,
        origin: Point,
    ) -> Cell {
        Self {
            kind,
            color,
            affine,
            origin,
        }
    }
}

impl Player {
    const DIMENSION: f64 = Cell::DIM * 17.0;

    const RADIUS: f64 = Cell::DIM * 0.6;

    const COLORS: [Color; 4] = [css::RED, css::YELLOW, css::BLUE, css::GREEN];

    const fn new(color: Color, affine: Affine) -> Self {
        Player { color, affine }
    }

    fn color(index: usize) -> Color {
        Self::COLORS[index % Self::COLORS.len()]
    }

    fn create_scene() -> Result<Scene> {
        let mut scene = Scene::new();
        let players = [
            Player::new(Self::COLORS[0], Affine::IDENTITY),
            Player::new(
                Self::COLORS[1],
                Affine::rotate(PI / 2.0)
                    .then_translate((Self::DIMENSION, 0.0).into()),
            ),
            Player::new(
                Self::COLORS[2],
                Affine::rotate(PI)
                    .then_translate((Self::DIMENSION, Self::DIMENSION).into()),
            ),
            Player::new(
                Self::COLORS[3],
                Affine::rotate(PI * 3.0 / 2.0)
                    .then_translate((0.0, Self::DIMENSION).into()),
            ),
        ];
        for (i, player) in players.iter().enumerate() {
            let mut cells = vec![];
            let mut origin = Point::new(Cell::DIM_X2, Cell::DIM_X4);
            let mut color_index = i + Self::COLORS.len() - 1;
            cells.push(Cell::new(
                CellKind::Triangle180,
                Self::color(color_index),
                player.affine,
                origin,
            ));
            color_index += 1;
            cells.push(Cell::new(
                CellKind::VBlock,
                Self::color(color_index),
                player.affine,
                origin,
            ));
            origin += (Cell::DIM, 0.0);
            color_index += 1;
            cells.push(Cell::new(
                CellKind::VBlock,
                Self::color(color_index),
                player.affine,
                origin,
            ));
            origin += (Cell::DIM, 0.0);
            color_index += 1;
            cells.push(Cell::new(
                CellKind::Triangle270,
                Self::color(color_index),
                player.affine,
                origin,
            ));
            color_index += 1;
            cells.push(Cell::new(
                CellKind::Triangle90,
                Self::color(color_index),
                player.affine,
                origin,
            ));
            origin += (0.0, -Cell::DIM);
            color_index += 1;
            cells.push(Cell::new(
                CellKind::HBlock,
                Self::color(color_index),
                player.affine,
                origin,
            ));
            origin += (0.0, -Cell::DIM);
            color_index += 1;
            cells.push(Cell::new(
                CellKind::HBlock,
                Self::color(color_index),
                player.affine,
                origin,
            ));
            origin += (Cell::DIM_X2, -Cell::DIM_X2);
            color_index += 1;
            cells.push(Cell::new(
                CellKind::Triangle180,
                Self::color(color_index),
                player.affine,
                origin,
            ));
            for _ in 0..5 {
                color_index += 1;
                cells.push(Cell::new(
                    CellKind::VBlock,
                    Self::color(color_index),
                    player.affine,
                    origin,
                ));
                origin += (Cell::DIM, 0.0);
            }
            for cell in cells {
                cell.draw(&mut scene);
            }
            player.draw(&mut scene);
        }
        Ok(scene)
    }
}

impl Drawable for Player {
    fn draw(&self, scene: &mut Scene) {
        scene.fill(
            Fill::NonZero,
            self.affine,
            self.color,
            None,
            &Rect::from_origin_size(
                Point::ORIGIN,
                (Cell::DIM_X4, Cell::DIM_X4),
            ),
        );
        let p = Point::new(Cell::DIM, Cell::DIM);
        for center in [
            p,
            p + (0.0, Cell::DIM_X2),
            p + (Cell::DIM_X2, 0.0),
            p + (Cell::DIM_X2, Cell::DIM_X2),
        ] {
            scene.fill(
                Fill::NonZero,
                self.affine,
                css::WHITE,
                None,
                &Circle::new(center, Self::RADIUS),
            );
        }
        let mut p = Point::new(0.0, Cell::DIM_X4 + Cell::DIM_X2);
        let mut path = BezPath::new();
        path.move_to(p);
        p += (Cell::DIM_X2, -Cell::DIM_X2);
        path.line_to(p);
        p += (Cell::DIM_X2, 0.0);
        path.line_to(p);
        p += (0.0, -Cell::DIM_X2);
        path.line_to(p);
        p += (Cell::DIM_X2, -Cell::DIM_X2);
        path.line_to(p);
        p += (Cell::DIM * 5.0, 0.0);
        path.line_to(p);
        scene.stroke(&Stroke::new(5.0), self.affine, css::BLACK, None, &path);

        let mut path = BezPath::new();
        let mut p = Point::new(Cell::DIM_X2, Cell::DIM_X4 + Cell::DIM_X2);
        path.move_to(p);
        p += (Cell::DIM_X4, 0.0);
        path.line_to(p);
        p += (0.0, -Cell::DIM_X4);
        path.line_to(p);
        p += (Cell::DIM * 5.0, 0.0);
        path.line_to(p);
        scene.stroke(&Stroke::new(5.0), self.affine, css::BLACK, None, &path);

        let mut path = BezPath::new();
        let mut p = Point::new(Cell::DIM_X2, Cell::DIM_X4 * 2.0);
        path.move_to(p);
        p += (Cell::DIM * 5.0, 0.0);
        path.line_to(p);
        p -= (0.0, Cell::DIM);
        path.line_to(p);
        path.line_to(p + (Cell::DIM * 1.5, Cell::DIM * 1.5));
        p += (0.0, Cell::DIM * 3.0);
        path.line_to(p);
        p -= (0.0, Cell::DIM);
        path.line_to(p);
        p -= (Cell::DIM * 5.0, 0.0);
        path.line_to(p);
        path.close_path();
        scene.fill(Fill::NonZero, self.affine, self.color, None, &path);
        scene.stroke(&Stroke::new(5.0), self.affine, css::BLACK, None, &path);

        let mut p = Point::new(Cell::DIM * 2.5, Cell::DIM * 8.5);
        for _ in 0..6 {
            scene.fill(
                Fill::NonZero,
                self.affine,
                css::WHITE,
                None,
                &Circle::new(p, Cell::RADIUS),
            );
            p += (Cell::DIM, 0.0);
        }
    }
}

impl Drawable for Cell {
    fn draw(&self, scene: &mut Scene) {
        let origin = self.origin;
        let mut center = Point::ZERO;
        let shape = match self.kind {
            CellKind::Triangle0 => Triangle::new(
                origin,
                origin + (Self::DIM_X2, 0.0),
                origin + (0.0, Self::DIM_X2),
            )
            .into(),
            CellKind::Triangle90 => Triangle::new(
                origin,
                origin + (Self::DIM_X2, 0.0),
                origin + (Self::DIM_X2, Self::DIM_X2),
            )
            .into(),
            CellKind::Triangle180 => Triangle::new(
                origin,
                origin + (0.0, Self::DIM_X2),
                origin + (-Self::DIM_X2, Self::DIM_X2),
            )
            .into(),
            CellKind::Triangle270 => Triangle::new(
                origin,
                origin + (Self::DIM_X2, Self::DIM_X2),
                origin + (0.0, Self::DIM_X2),
            )
            .into(),
            _ => None,
        };
        if let Some(shape) = shape.as_ref() {
            scene.fill(Fill::NonZero, self.affine, self.color, None, shape);
            center = shape.inscribed_circle().center;
        }
        let shape = match self.kind {
            CellKind::VBlock => {
                Rect::from_origin_size(origin, (Self::DIM, Self::DIM_X2)).into()
            }
            CellKind::HBlock => {
                Rect::from_origin_size(origin, (Self::DIM_X2, Self::DIM)).into()
            }
            _ => None,
        };
        if let Some(shape) = shape.as_ref() {
            scene.fill(Fill::NonZero, self.affine, self.color, None, shape);
            center = shape.center();
        }
        scene.fill(
            Fill::NonZero,
            self.affine,
            css::WHITE,
            None,
            &Circle::new(center, Cell::RADIUS),
        );
    }
}
