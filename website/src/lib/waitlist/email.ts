function escapeHtml(input: string): string {
	return input
		.replaceAll("&", "&amp;")
		.replaceAll("<", "&lt;")
		.replaceAll(">", "&gt;")
		.replaceAll('"', "&quot;")
		.replaceAll("'", "&#39;");
}

function button(label: string, url: string, background: string, color: string): string {
	return `
		<a
			href="${escapeHtml(url)}"
			style="
				display:inline-block;
				padding:14px 22px;
				border-radius:999px;
				background:${background};
				color:${color};
				font-size:15px;
				font-weight:700;
				letter-spacing:0.01em;
				text-decoration:none;
			"
		>
			${escapeHtml(label)}
		</a>
	`;
}

export function buildWaitlistEmail(input: {
	logoUrl: string;
	confirmUrl: string;
	unsubscribeUrl: string;
}): { subject: string; html: string; text: string } {
	const subject = "Confirm your Waddle waitlist request";
	const confirmUrl = escapeHtml(input.confirmUrl);
	const unsubscribeUrl = escapeHtml(input.unsubscribeUrl);
	const logoUrl = escapeHtml(input.logoUrl);
	const preheader = "Confirm your spot on the Waddle waitlist.";

	const html = `<!doctype html>
<html lang="en">
	<body style="margin:0;padding:0;background:#e9f5f7;color:#0f252a;">
		<div style="display:none;max-height:0;overflow:hidden;opacity:0;">
			${escapeHtml(preheader)}
		</div>
		<table role="presentation" width="100%" cellspacing="0" cellpadding="0" style="background:#e9f5f7;">
			<tr>
				<td align="center" style="padding:28px 12px;">
					<table
						role="presentation"
						width="100%"
						cellspacing="0"
						cellpadding="0"
						style="
							max-width:640px;
							background:#ffffff;
							border:1px solid #cfe5e7;
							border-radius:28px;
							overflow:hidden;
						"
					>
						<tr>
							<td style="padding:32px 32px 18px;background:#f6fbfc;">
								<table role="presentation" width="100%" cellspacing="0" cellpadding="0">
									<tr>
										<td style="padding-bottom:18px;">
											<img
												src="${logoUrl}"
												alt="Waddle"
												width="96"
												height="96"
												style="display:block;width:96px;height:96px;border:0;"
											/>
										</td>
									</tr>
									<tr>
										<td
											style="
												font-family:Arial, 'Helvetica Neue', Helvetica, sans-serif;
												font-size:12px;
												font-weight:700;
												letter-spacing:0.14em;
												text-transform:uppercase;
												color:#4d7b82;
												padding-bottom:12px;
											"
										>
											Private beta dispatch
										</td>
									</tr>
									<tr>
										<td
											style="
												font-family:Arial, 'Helvetica Neue', Helvetica, sans-serif;
												font-size:32px;
												line-height:1.12;
												font-weight:700;
												color:#0f252a;
												padding-bottom:14px;
											"
										>
											Confirm your place on the Waddle waitlist.
										</td>
									</tr>
									<tr>
										<td
											style="
												font-family:Arial, 'Helvetica Neue', Helvetica, sans-serif;
												font-size:16px;
												line-height:1.6;
												color:#38555b;
											"
										>
											We use a confirmation step so nobody can quietly add someone else’s inbox.
											If this request was yours, confirm it below. If not, cancel it with one click.
										</td>
									</tr>
								</table>
							</td>
						</tr>
						<tr>
							<td style="padding:28px 32px 16px;">
								<table role="presentation" width="100%" cellspacing="0" cellpadding="0">
									<tr>
										<td style="padding-bottom:14px;">
											${button("Confirm waitlist signup", input.confirmUrl, "#10262b", "#f8fffe")}
										</td>
									</tr>
									<tr>
										<td style="padding-bottom:24px;">
											${button("Cancel this request", input.unsubscribeUrl, "#edf5f6", "#1f4045")}
										</td>
									</tr>
									<tr>
										<td
											style="
												font-family:Arial, 'Helvetica Neue', Helvetica, sans-serif;
												font-size:14px;
												line-height:1.6;
												color:#5d787d;
												padding-bottom:10px;
											"
										>
											Prefer plain links?
										</td>
									</tr>
									<tr>
										<td
											style="
												font-family:Arial, 'Helvetica Neue', Helvetica, sans-serif;
												font-size:13px;
												line-height:1.7;
												color:#1f4045;
												word-break:break-word;
											"
										>
											<strong>Confirm:</strong><br />
											<a href="${confirmUrl}" style="color:#25626b;text-decoration:underline;">${confirmUrl}</a>
											<br /><br />
											<strong>Cancel:</strong><br />
											<a href="${unsubscribeUrl}" style="color:#25626b;text-decoration:underline;">${unsubscribeUrl}</a>
										</td>
									</tr>
								</table>
							</td>
						</tr>
						<tr>
							<td
								style="
									padding:18px 32px 30px;
									font-family:Arial, 'Helvetica Neue', Helvetica, sans-serif;
									font-size:13px;
									line-height:1.7;
									color:#6b8589;
									border-top:1px solid #e1eff1;
								"
							>
								Waddle sends one confirmation email first, then only the launch notes you asked for.
								If you never requested this, use the cancel link and you’re done.
							</td>
						</tr>
					</table>
				</td>
			</tr>
		</table>
	</body>
</html>`;

	const text = [
		"Confirm your Waddle waitlist request",
		"",
		"We use a confirmation step so nobody can quietly add someone else’s inbox.",
		"",
		`Confirm your signup: ${input.confirmUrl}`,
		"",
		`Cancel this request: ${input.unsubscribeUrl}`,
		"",
		"Waddle sends one confirmation email first, then only the launch notes you asked for.",
	].join("\n");

	return { subject, html, text };
}
