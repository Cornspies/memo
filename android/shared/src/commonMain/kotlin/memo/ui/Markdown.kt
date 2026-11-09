package memo.ui

import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.rememberTransformableState
import androidx.compose.foundation.gestures.transformable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.IntrinsicSize
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Checkbox
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.LinkAnnotation
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.TextLinkStyles
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.text.withLink
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties

/** What a rendered body needs from the memo it belongs to. */
class RenderContext(
    val app: AppState,
    val memoId: String,
    val attachments: List<AttachmentView>,
    /** Null when the body is read-only (or not the memo's current one). */
    val onToggle: ((Int) -> Unit)?,
)

@Composable
fun Markdown(ctx: RenderContext, body: String, modifier: Modifier = Modifier) {
    var blocks by remember { mutableStateOf<List<Block>?>(null) }
    LaunchedEffect(body) {
        blocks =
            try {
                ctx.app.backend.render(body)
            } catch (e: BackendException) {
                listOf(Block.Code(body))
            }
    }
    Column(modifier, verticalArrangement = Arrangement.spacedBy(10.dp)) {
        blocks?.forEach { Block(ctx, it) }
    }
}

@Composable
private fun Block(ctx: RenderContext, block: Block) {
    val type = MaterialTheme.typography
    when (block) {
        is Block.Paragraph -> Inlines(ctx, block.content, type.bodyLarge)
        is Block.Heading ->
            Inlines(
                ctx,
                block.content,
                when (block.level) {
                    1 -> type.headlineSmall
                    2 -> type.titleLarge
                    3 -> type.titleMedium
                    else -> type.titleSmall
                },
            )
        is Block.Quote ->
            Row(Modifier.height(IntrinsicSize.Min)) {
                Box(Modifier.width(3.dp).fillMaxHeight().background(MaterialTheme.colorScheme.outlineVariant))
                Column(Modifier.padding(start = 12.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    block.blocks.forEach { Block(ctx, it) }
                }
            }
        is Block.ListBlock ->
            Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                block.items.forEachIndexed { i, item ->
                    Row(verticalAlignment = Alignment.Top) {
                        val task = item.task
                        if (task != null) {
                            Checkbox(
                                task.checked,
                                onCheckedChange = ctx.onToggle?.let { toggle -> { toggle(task.offset) } },
                                modifier = Modifier.size(28.dp).padding(end = 6.dp),
                            )
                        } else {
                            Text(
                                block.start?.let { "${it + i}." } ?: "•",
                                style = type.bodyLarge,
                                modifier = Modifier.widthIn(min = 22.dp).padding(end = 4.dp),
                            )
                        }
                        Column(verticalArrangement = Arrangement.spacedBy(4.dp)) { item.blocks.forEach { Block(ctx, it) } }
                    }
                }
            }
        is Block.Code ->
            Text(
                block.text,
                style = type.bodyMedium.copy(fontFamily = FontFamily.Monospace),
                modifier =
                    Modifier.fillMaxWidth().clip(RoundedCornerShape(6.dp))
                        .background(MaterialTheme.colorScheme.surfaceContainerHigh)
                        .horizontalScroll(rememberScrollState()).padding(10.dp),
                softWrap = false,
            )
        is Block.Table ->
            Column(
                Modifier.horizontalScroll(rememberScrollState())
                    .border(1.dp, MaterialTheme.colorScheme.outlineVariant, RoundedCornerShape(4.dp)),
            ) {
                val bold = type.bodyMedium.copy(fontWeight = FontWeight.SemiBold)
                Row { block.head.forEach { Inlines(ctx, it, bold, Modifier.width(140.dp).padding(8.dp)) } }
                for (row in block.rows) {
                    HorizontalDivider()
                    Row { row.forEach { Inlines(ctx, it, type.bodyMedium, Modifier.width(140.dp).padding(8.dp)) } }
                }
            }
        Block.Rule -> HorizontalDivider()
    }
}

/** Text runs, with images on their own lines in between. */
@Composable
private fun Inlines(ctx: RenderContext, content: List<Inline>, style: TextStyle, modifier: Modifier = Modifier) {
    val runs = mutableListOf<MutableList<Inline>>(mutableListOf())
    val images = mutableListOf<Inline.Image?>(null)
    for (inline in content) {
        if (inline is Inline.Image) {
            images.add(inline)
            runs.add(mutableListOf())
        } else {
            runs.last().add(inline)
        }
    }
    Column(modifier, verticalArrangement = Arrangement.spacedBy(6.dp)) {
        runs.forEachIndexed { i, run ->
            images[i]?.let { AttachmentImage(ctx, it) }
            val text = annotated(ctx, run.dropWhile { it == Inline.Break || (it is Inline.Text && it.text.isBlank()) })
            if (text.isNotEmpty()) Text(text, style = style)
        }
    }
}

@Composable
private fun annotated(ctx: RenderContext, run: List<Inline>): AnnotatedString {
    val colors = MaterialTheme.colorScheme
    val linkStyle = TextLinkStyles(SpanStyle(color = colors.primary, textDecoration = TextDecoration.Underline))
    return buildAnnotatedString {
        for (inline in run) {
            when (inline) {
                is Inline.Text -> {
                    val span =
                        SpanStyle(
                            fontWeight = if (inline.bold) FontWeight.Bold else null,
                            fontStyle = if (inline.italic) FontStyle.Italic else null,
                            textDecoration = if (inline.strike) TextDecoration.LineThrough else null,
                            fontFamily = if (inline.code) FontFamily.Monospace else null,
                            background = if (inline.code) colors.surfaceContainerHigh else Color.Unspecified,
                        )
                    val link = inline.link
                    if (link != null) {
                        withLink(LinkAnnotation.Url(link, linkStyle)) { withStyle(span) { append(inline.text) } }
                    } else {
                        withStyle(span) { append(inline.text) }
                    }
                }
                is Inline.Tag ->
                    withLink(
                        LinkAnnotation.Clickable(inline.tag, TextLinkStyles(SpanStyle(color = colors.primary))) {
                            ctx.app.filter = MemoFilter(tag = inline.tag)
                            ctx.app.home()
                        },
                    ) { append("#${inline.name}") }
                Inline.Break -> append("\n")
                is Inline.Image -> {}
            }
        }
    }
}

@Composable
private fun AttachmentImage(ctx: RenderContext, image: Inline.Image) {
    val view = ctx.attachments.find { it.id == image.attachment }
    if (view == null) {
        Text("[image not found]", color = MaterialTheme.colorScheme.onSurfaceVariant)
        return
    }
    val version by ctx.app.backend.version.collectAsState()
    var bitmap by remember(view.id) { mutableStateOf<ImageBitmap?>(null) }
    var failed by remember(view.id) { mutableStateOf<String?>(null) }
    var open by remember { mutableStateOf(false) }
    val kind = if (view.thumb != null) "thumbnail" else "original"
    LaunchedEffect(view.id, version) {
        if (bitmap != null) return@LaunchedEffect
        try {
            bitmap = ctx.app.backend.image(ctx.memoId, view.id, kind)
            failed = null
        } catch (e: BackendException) {
            failed = errorText(e)
        }
    }
    val ratio = if (view.width > 0 && view.height > 0) view.width.toFloat() / view.height else 1.5f
    Box(
        Modifier.widthIn(max = 520.dp).fillMaxWidth().aspectRatio(ratio).clip(RoundedCornerShape(6.dp))
            .background(MaterialTheme.colorScheme.surfaceContainerHigh).clickable(bitmap != null) { open = true },
        contentAlignment = Alignment.Center,
    ) {
        val b = bitmap
        if (b != null) {
            Image(b, image.alt, Modifier.fillMaxSize(), contentScale = ContentScale.Fit)
        } else {
            Text(
                failed ?: if (view.thumbAvailable || view.available) "Loading…" else "Not uploaded yet",
                style = MaterialTheme.typography.labelMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
    if (open) Lightbox(ctx, view, image.alt) { open = false }
}

@Composable
private fun Lightbox(ctx: RenderContext, view: AttachmentView, alt: String, onClose: () -> Unit) {
    var bitmap by remember { mutableStateOf<ImageBitmap?>(null) }
    var failed by remember { mutableStateOf<String?>(null) }
    LaunchedEffect(view.id) {
        try {
            bitmap = ctx.app.backend.image(ctx.memoId, view.id, "original")
            if (bitmap == null) failed = "The full image is not available yet."
        } catch (e: BackendException) {
            failed = errorText(e)
        }
    }
    var scale by remember { mutableFloatStateOf(1f) }
    var offset by remember { mutableStateOf(Offset.Zero) }
    val transform = rememberTransformableState { _, zoom, pan, _ ->
        scale = (scale * zoom).coerceIn(1f, 8f)
        offset = if (scale == 1f) Offset.Zero else offset + pan
    }
    Dialog(onClose, DialogProperties(usePlatformDefaultWidth = false)) {
        Box(Modifier.fillMaxSize().background(Color.Black).clickable(onClick = onClose), contentAlignment = Alignment.Center) {
            val b = bitmap
            if (b != null) {
                Image(
                    b,
                    alt,
                    Modifier.fillMaxSize().transformable(transform).graphicsLayer {
                        scaleX = scale
                        scaleY = scale
                        translationX = offset.x
                        translationY = offset.y
                    },
                    contentScale = ContentScale.Fit,
                )
            } else {
                Text(failed ?: "Loading…", color = Color.White)
            }
            Text(
                "${view.width} × ${view.height}",
                color = Color.White.copy(alpha = 0.7f),
                style = MaterialTheme.typography.labelSmall,
                modifier = Modifier.align(Alignment.BottomCenter).padding(24.dp),
            )
        }
    }
}
