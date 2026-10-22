use super::*;

pub(crate) fn compose_runtime_frame(runtime: &mut RuntimeEngine, renderer: &mut Renderer<'_>, graph_textures: &mut BTreeMap<String, CachedGraphTexture>, mut commands: Vec<RenderCommand>) -> Vec<RenderCommand> {
    let frame_draw_items = runtime.api.graph_output_draw_items();
for key in frame_draw_items
    .iter()
    .map(|item| item.key.as_str())
    .collect::<BTreeSet<_>>()
{
    let Some(image) = runtime.api.graph_images.get(key) else {
        continue;
    };
    let revision = runtime.api.graph_image_revision(key);
    match graph_textures.get_mut(key) {
        Some(cached) if cached.revision != revision => {
            match renderer.update_rgba(&cached.handle, image) {
                Ok(handle) => {
                    cached.handle = handle;
                    cached.revision = revision;
                    tracing::debug!(key, revision, "updated graph texture");
                }
                Err(err) => tracing::warn!(
                    key,
                    revision,
                    %err,
                    "graph texture update failed"
                ),
            }
        }
        Some(_) => {}
        None => match renderer.insert_rgba(image) {
            Ok(handle) => {
                tracing::info!(
                    key,
                    revision,
                    width = handle.width,
                    height = handle.height,
                    "uploaded graph texture"
                );
                graph_textures.insert(
                    key.to_string(),
                    CachedGraphTexture { handle, revision },
                );
            }
            Err(err) => {
                tracing::warn!(key, %err, "graph texture upload failed")
            }
        },
    }
}
// `graph_output_draw_items()` preserves ordinary raw-priority
// order and performs only the recovered BackF exception for
// projected Sprite modes 5/6.  The renderer performs a stable
// sort by z, so a projected sprite deliberately moved after a
// higher-priority BackF must inherit the preceding renderer z;
// otherwise the sort would put it behind BackF again.  Normal
// backgrounds and transition images remain monotonic and keep
// their raw z, allowing BackF to cover/fade them as in the
// target compositor.
let mut flattened_graph_z = i32::MIN;
for item in frame_draw_items {
    let Some(cached) = graph_textures.get(&item.key) else {
        continue;
    };
    let raw_graph_z = item.z;
    let render_graph_z = raw_graph_z.max(flattened_graph_z);
    if render_graph_z != raw_graph_z {
        tracing::debug!(
            owner = ?item.owner_object,
            key = item.key,
            raw_graph_z,
            render_graph_z,
            "promoted graph render z to preserve flattened native display order"
        );
    }
    flattened_graph_z = render_graph_z;
    let texture = &cached.handle;
    commands.push(RenderCommand::DrawTexture {
        texture: texture.id,
        x: item.x,
        y: item.y,
        width: item.width,
        height: item.height,
        src_x: item.src_x / texture.width as f32,
        src_y: item.src_y / texture.height as f32,
        src_width: item.src_width / texture.width as f32,
        src_height: item.src_height / texture.height as f32,
        opacity: item.opacity,
        ignore_source_alpha: item.ignore_source_alpha,
        blend_mode: item.blend_mode,
        rotation_degrees: item.rotation_degrees,
        destination_quad: item.destination_quad,
        linear_sampling: item.linear_sampling,
        clip: item
            .clip
            .map(|clip| [clip.x, clip.y, clip.width, clip.height]),
        z: 2 + render_graph_z,
    });
}
{
    for node in runtime
        .api
        .text_nodes
        .values()
        .filter(|node| runtime.api.should_draw_text_node(node))
    {
        let parent = node
            .target_surface
            .filter(|surface| runtime.api.graph_surfaces.contains_key(surface))
            .map(|surface| {
                let (x, y) = runtime.api.surface_world_position(surface);
                (x, y, runtime.api.surface_display_opacity(surface))
            })
            .unwrap_or((0.0, 0.0, 1.0));
        let layout = runtime.api.formatted_text_layout(node);
        let x = parent.0 + node.x;
        let cursor_y = parent.1 + node.y;
        let y = cursor_y + layout.body_y_offset;
        let clip = node
            .target_surface
            .and_then(|surface| runtime.api.surface_chain_clip(surface))
            .map(|clip| [clip.x, clip.y, clip.width, clip.height]);
        let size = node.size.max(12.0);
        let mut color = node.color;
        color[3] *= parent.2;
        let styles = node
            .style_spans
            .iter()
            .map(|span| TextStyleSpan {
                start_char: span.start_char,
                end_char: span.end_char,
                packed_rgb: span.style.packed_rgb,
                bold: span.style.bold,
                italic: span.style.italic,
            })
            .collect::<Vec<_>>();
        let shadow_styles = styles
            .iter()
            .map(|span| TextStyleSpan {
                packed_rgb: None,
                ..*span
            })
            .collect::<Vec<_>>();
        if let Some((shadow_x, shadow_y, shadow_alpha)) =
            runtime.api.graph_defaults.shadow_for_height(size)
        {
            commands.push(RenderCommand::DrawText {
                text: node.text.clone(),
                x: x + shadow_x as f32,
                y: y + shadow_y as f32,
                color: [0.0, 0.0, 0.0, color[3] * shadow_alpha],
                size,
                styles: shadow_styles,
                clip,
                z: node.z - 1,
            });
        }
        commands.push(RenderCommand::DrawText {
            text: node.text.clone(),
            x,
            y,
            color,
            size,
            styles,
            clip,
            z: node.z,
        });
        let mut positioned_node = node.clone();
        positioned_node.x = x;
        positioned_node.y = cursor_y;
        for (ruby, ruby_x, ruby_y, ruby_size) in
            text::ruby_draw_runs(&positioned_node, layout)
        {
            if let Some((shadow_x, shadow_y, shadow_alpha)) =
                runtime.api.graph_defaults.shadow_for_height(ruby_size)
            {
                commands.push(RenderCommand::DrawText {
                    text: ruby.clone(),
                    x: ruby_x + shadow_x as f32,
                    y: ruby_y + shadow_y as f32,
                    color: [0.0, 0.0, 0.0, color[3] * shadow_alpha],
                    size: ruby_size,
                    styles: Vec::new(),
                    clip,
                    z: node.z,
                });
            }
            commands.push(RenderCommand::DrawText {
                text: ruby,
                x: ruby_x,
                y: ruby_y,
                color,
                size: ruby_size,
                styles: Vec::new(),
                clip,
                z: node.z + 1,
            });
        }
    }
}
{
    for (line, x, y, size) in runtime.api.user_modeless_dialog_text_lines() {
        commands.push(RenderCommand::DrawText {
            text: line.clone(),
            x: x + 2.0,
            y: y + 2.0,
            color: [0.0, 0.0, 0.0, 0.9],
            size,
            styles: Vec::new(),
            clip: None,
            z: 29_999,
        });
        commands.push(RenderCommand::DrawText {
            text: line,
            x,
            y,
            color: [0.95, 0.95, 0.95, 1.0],
            size,
            styles: Vec::new(),
            clip: None,
            z: 30_000,
        });
    }
}
commands
}
