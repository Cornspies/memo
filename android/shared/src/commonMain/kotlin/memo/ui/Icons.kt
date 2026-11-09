package memo.ui

import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.StrokeJoin
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.graphics.vector.PathBuilder
import androidx.compose.ui.graphics.vector.path
import androidx.compose.ui.unit.dp

// A handful of line icons, so the app does not need an icon library.

private fun icon(name: String, width: Float = 2f, draw: PathBuilder.() -> Unit): ImageVector =
    ImageVector.Builder(name, 24.dp, 24.dp, 24f, 24f).apply {
        path(
            stroke = SolidColor(Color.Black),
            strokeLineWidth = width,
            strokeLineCap = StrokeCap.Round,
            strokeLineJoin = StrokeJoin.Round,
            pathBuilder = draw,
        )
    }.build()

private fun PathBuilder.circle(x: Float, y: Float, r: Float) {
    moveTo(x - r, y)
    arcTo(r, r, 0f, true, true, x + r, y)
    arcTo(r, r, 0f, true, true, x - r, y)
}

object Icons {
    val Back = icon("back") {
        moveTo(19f, 12f)
        lineTo(5f, 12f)
        moveTo(11f, 6f)
        lineTo(5f, 12f)
        lineTo(11f, 18f)
    }
    val Add = icon("add") {
        moveTo(12f, 5f)
        lineTo(12f, 19f)
        moveTo(5f, 12f)
        lineTo(19f, 12f)
    }
    val More = icon("more", width = 3f) {
        for (y in listOf(5f, 12f, 19f)) {
            moveTo(12f, y)
            lineTo(12f, y + 0.01f)
        }
    }
    val Check = icon("check") {
        moveTo(5f, 12.5f)
        lineTo(10f, 17.5f)
        lineTo(19f, 7f)
    }
    val Edit = icon("edit") {
        moveTo(4f, 20f)
        lineTo(8f, 19f)
        lineTo(19f, 8f)
        lineTo(16f, 5f)
        lineTo(5f, 16f)
        close()
        moveTo(14f, 7f)
        lineTo(17f, 10f)
    }
    val Image = icon("image") {
        moveTo(4f, 5f)
        lineTo(20f, 5f)
        lineTo(20f, 19f)
        lineTo(4f, 19f)
        close()
        moveTo(4f, 16f)
        lineTo(9f, 11f)
        lineTo(14f, 16f)
        moveTo(13f, 15f)
        lineTo(16f, 12f)
        lineTo(20f, 16f)
        circle(15.5f, 8.5f, 1.2f)
    }
    val Search = icon("search") {
        circle(10.5f, 10.5f, 6f)
        moveTo(15f, 15f)
        lineTo(20f, 20f)
    }
    val Close = icon("close") {
        moveTo(6f, 6f)
        lineTo(18f, 18f)
        moveTo(18f, 6f)
        lineTo(6f, 18f)
    }
}

@Composable
fun IconAction(icon: ImageVector, label: String, enabled: Boolean = true, onClick: () -> Unit) {
    IconButton(onClick, enabled = enabled) { Icon(icon, contentDescription = label) }
}
