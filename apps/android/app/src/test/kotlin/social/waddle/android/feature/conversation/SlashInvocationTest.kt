package social.waddle.android.feature.conversation

import org.junit.Assert.assertEquals
import org.junit.Test
import social.waddle.android.client.ExtensionCommand
import social.waddle.android.client.ExtensionCommandScope

/** The four-way `buildSlashInvocation` dispatch table (web parity). */
class SlashInvocationTest {
    private fun command(
        inlineField: String? = null,
        composerExecute: Boolean = false,
    ) = ExtensionCommand(
        serviceJid = "extensions.waddle.test",
        node = "urn:waddle:extension:1:test",
        name = "Test",
        scope = ExtensionCommandScope.GLOBAL,
        composerPrefix = "test",
        inlineField = inlineField,
        composerExecute = composerExecute,
    )

    @Test
    fun `inline field plus trailing text is an inline submit`() {
        val ai = command(inlineField = "prompt")
        assertEquals(
            SlashInvocation.InlineSubmit(command = ai, fieldName = "prompt", value = "hello"),
            buildSlashInvocation(ai, "  hello  "),
        )
    }

    @Test
    fun `composer-execute without trailing text executes directly`() {
        val stargate = command(composerExecute = true)
        assertEquals(
            SlashInvocation.DirectExecute(command = stargate),
            buildSlashInvocation(stargate, ""),
        )
    }

    @Test
    fun `trailing text without an inline field opens the palette with a prefill`() {
        val poll = command()
        assertEquals(
            SlashInvocation.OpenPalette(command = poll, prefillFirstRequired = "Lunch?"),
            buildSlashInvocation(poll, "Lunch?"),
        )
    }

    @Test
    fun `trailing text beats composer-execute`() {
        val stargate = command(composerExecute = true)
        assertEquals(
            SlashInvocation.OpenPalette(command = stargate, prefillFirstRequired = "extra"),
            buildSlashInvocation(stargate, "extra"),
        )
    }

    @Test
    fun `a bare command opens the palette`() {
        val poll = command()
        assertEquals(
            SlashInvocation.OpenPalette(command = poll),
            buildSlashInvocation(poll, ""),
        )
    }

    @Test
    fun `an inline field with blank trailing text opens the palette`() {
        val ai = command(inlineField = "prompt")
        assertEquals(
            SlashInvocation.OpenPalette(command = ai),
            buildSlashInvocation(ai, "   "),
        )
    }
}
