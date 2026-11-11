package memo.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp

private val Weekdays = listOf("M", "T", "W", "T", "F", "S", "S")

/**
 * A month of days, shaded by how many memos were created on each (`counts`, by local midnight).
 * Days without memos cannot be picked; picking the selected day again clears it.
 */
@Composable
fun MonthCalendar(platform: Platform, counts: Map<Long, Int>, selected: Long?, onPick: (Long?) -> Unit) {
    val today = remember { platform.today() }
    var month by remember { mutableLongStateOf(platform.monthStart(selected ?: today)) }
    val colors = MaterialTheme.colorScheme
    // The month's days, padded to whole weeks starting on Monday.
    val cells = buildList<Long?> {
        repeat(platform.weekday(month) - 1) { add(null) }
        var day = month
        while (platform.monthStart(day) == month) {
            add(day)
            day = platform.addDays(day, 1)
        }
        while (size % 7 != 0) add(null)
    }
    Column(Modifier.widthIn(max = 420.dp).fillMaxWidth().padding(horizontal = 16.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            TextButton({ month = platform.monthStart(month, -1) }) { Text("‹") }
            Text(
                platform.monthName(month),
                style = MaterialTheme.typography.titleSmall,
                modifier = Modifier.weight(1f),
                textAlign = TextAlign.Center,
            )
            TextButton({ month = platform.monthStart(month, 1) }) { Text("›") }
        }
        Row {
            for (name in Weekdays) {
                Text(
                    name,
                    style = MaterialTheme.typography.labelSmall,
                    color = colors.onSurfaceVariant,
                    textAlign = TextAlign.Center,
                    modifier = Modifier.weight(1f),
                )
            }
        }
        for (week in cells.chunked(7)) {
            Row(horizontalArrangement = Arrangement.spacedBy(2.dp), modifier = Modifier.padding(vertical = 1.dp)) {
                for (day in week) {
                    Box(Modifier.weight(1f).aspectRatio(1.4f), contentAlignment = Alignment.Center) {
                        if (day != null) {
                            val count = counts[day] ?: 0
                            val picked = day == selected
                            val shade =
                                when {
                                    picked -> colors.primary
                                    count == 0 -> Color.Transparent
                                    count == 1 -> colors.primary.copy(alpha = 0.22f)
                                    count <= 3 -> colors.primary.copy(alpha = 0.45f)
                                    else -> colors.primary.copy(alpha = 0.7f)
                                }
                            var cell =
                                Modifier.fillMaxWidth().aspectRatio(1.4f).clip(RoundedCornerShape(6.dp)).background(shade)
                                    .clickable(enabled = count > 0 || picked) { onPick(if (picked) null else day) }
                            if (day == today) cell = cell.border(1.dp, colors.primary, RoundedCornerShape(6.dp))
                            Box(cell, contentAlignment = Alignment.Center) {
                                Text(
                                    "${platform.dayOfMonth(day)}",
                                    style = MaterialTheme.typography.labelMedium,
                                    fontWeight = if (picked) FontWeight.Bold else null,
                                    color =
                                        when {
                                            picked || count > 3 -> colors.onPrimary
                                            count == 0 -> colors.onSurfaceVariant.copy(alpha = 0.6f)
                                            else -> colors.onSurface
                                        },
                                )
                            }
                        }
                    }
                }
            }
        }
    }
}
