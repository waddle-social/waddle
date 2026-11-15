package social.waddle.android.feature.events

import java.math.BigDecimal
import java.util.Currency
import java.util.Locale

/** Parse the displayed currency amount without rounding or floating-point arithmetic. */
internal fun planningAmountMinor(amount: String, currency: String): Long? = runCatching {
    val digits = Currency.getInstance(currency.trim().uppercase(Locale.ROOT)).defaultFractionDigits
    require(digits >= 0)
    val text = amount.trim()
    require(text.length <= 128 && text.matches(Regex("(?:[0-9]+(?:\\.[0-9]*)?|\\.[0-9]+)")))
    val decimal = BigDecimal(text)
    require(decimal.signum() >= 0)
    decimal.movePointRight(digits).longValueExact().also { require(it <= 9_007_199_254_740_991L) }
}.getOrNull()

/** Preserve the currency's precision when an existing expense is opened for editing. */
internal fun planningAmountText(amountMinor: Long?, currency: String): String {
    if (amountMinor == null) return ""
    return runCatching {
        val digits = Currency.getInstance(currency.trim().uppercase(Locale.ROOT)).defaultFractionDigits
        require(digits >= 0)
        BigDecimal.valueOf(amountMinor, digits).toPlainString()
    }.getOrDefault("")
}
