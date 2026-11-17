// Polar Huddle daylight tokens (design/src/preset.ts) resolved to hex, since
// mail clients support neither OKLCH nor CSS variables.
const color = {
	sea: "#f1f8ff", // sea (floe.50)
	surface: "#ffffff",
	surface2: "#edf0f4", // ice.100
	border: "#c8ced6",
	borderSoft: "#dbe0e6", // ice.200
	fg: "#060f19", // ice.975
	muted: "#38424e", // ice.700
	soft: "#525c67", // ice.600
	action: "#0f4376", // floe.800
	halo: "#205f9f", // floe.700
};

// Mail clients can't load the site's self-hosted fonts; name them first so
// clients with them installed match, then fall back to the design's stacks.
const font = {
	display: "'Bricolage Grotesque', 'Atkinson Hyperlegible Next', system-ui, -apple-system, 'Segoe UI', Roboto, sans-serif",
	body: "'Atkinson Hyperlegible Next', system-ui, -apple-system, 'Segoe UI', Roboto, 'Noto Sans', sans-serif",
};

function escapeHtml(input: string): string {
	return input
		.replaceAll("&", "&amp;")
		.replaceAll("<", "&lt;")
		.replaceAll(">", "&gt;")
		.replaceAll('"', "&quot;")
		.replaceAll("'", "&#39;");
}

// wd-btn: 40px tall, control radius, body 600 14px.
function button(label: string, url: string, primary: boolean): string {
	const tone = primary
		? `background:${color.action};border:1px solid ${color.action};color:#ffffff;`
		: `background:${color.surface};border:1px solid ${color.border};color:${color.fg};`;
	return `<a href="${escapeHtml(url)}" style="display:inline-block;margin:0 8px 8px 0;${tone}padding:0 16px;line-height:38px;border-radius:12px;font-family:${font.body};font-size:14px;font-weight:600;text-decoration:none;">${escapeHtml(label)}</a>`;
}

// Mirrors the "Supported by" cards on the landing page.
function sponsor(input: { href: string; logo: string; alt: string; width: number; title: string; caption: string }): string {
	return `
		<tr>
			<td style="padding:0 0 10px;">
				<a href="${input.href}" style="display:block;text-decoration:none;color:${color.fg};">
					<table role="presentation" width="100%" cellspacing="0" cellpadding="0" style="background:${color.surface};border:1px solid ${color.borderSoft};border-radius:16px;">
						<tr>
							<td width="${input.width}" style="padding:12px;width:${input.width}px;">
								<img src="${input.logo}" alt="${input.alt}" width="${input.width}" height="32" style="display:block;width:${input.width}px;height:32px;border:0;border-radius:6px;" />
							</td>
							<td style="padding:12px 16px 12px 4px;font-family:${font.body};">
								<div style="font-family:${font.display};font-size:15px;line-height:1.3;font-weight:700;color:${color.fg};">${input.title}</div>
								<div style="font-size:13px;line-height:18px;color:${color.muted};">${input.caption}</div>
							</td>
						</tr>
					</table>
				</a>
			</td>
		</tr>`;
}

export function buildWaitlistEmail(input: {
	origin: string;
	confirmUrl: string;
	unsubscribeUrl: string;
}): { subject: string; html: string; text: string } {
	const subject = "Confirm your Waddle waitlist request";
	const confirmUrl = escapeHtml(input.confirmUrl);
	const unsubscribeUrl = escapeHtml(input.unsubscribeUrl);
	const asset = (path: string) => escapeHtml(new URL(path, input.origin).toString());
	const siteUrl = asset("/");
	const preheader = "Confirm your spot on the Waddle waitlist.";

	const html = `<!doctype html>
<html lang="en">
	<head>
		<meta charset="utf-8" />
		<meta name="viewport" content="width=device-width, initial-scale=1" />
		<meta name="color-scheme" content="light" />
		<meta name="supported-color-schemes" content="light" />
	</head>
	<body style="margin:0;padding:0;background:${color.sea};color:${color.fg};">
		<div style="display:none;max-height:0;overflow:hidden;opacity:0;">${escapeHtml(preheader)}</div>
		<table role="presentation" width="100%" cellspacing="0" cellpadding="0" style="background:${color.sea};">
			<tr>
				<td align="center" style="padding:28px 12px 36px;">
					<table role="presentation" width="100%" cellspacing="0" cellpadding="0" style="max-width:560px;">
						<tr>
							<td style="padding:0 4px 20px;">
								<a href="${siteUrl}" style="text-decoration:none;color:${color.fg};">
									<img src="${asset("/waddle-mark.png")}" alt="" width="32" height="32" style="display:inline-block;width:32px;height:32px;padding:4px;border:0;border-radius:999px;background:#ffffff;vertical-align:middle;" />
									<span style="font-family:${font.display};font-size:18px;font-weight:700;letter-spacing:-0.01em;vertical-align:middle;">&nbsp;waddle.social</span>
								</a>
							</td>
						</tr>
						<tr>
							<td style="background:${color.surface};border:1px solid ${color.borderSoft};border-radius:20px;padding:32px;">
								<div style="font-family:${font.body};font-size:13px;line-height:18px;font-weight:700;letter-spacing:0.01em;color:${color.muted};padding-bottom:10px;">Private beta</div>
								<div style="font-family:${font.display};font-size:30px;line-height:1.15;font-weight:800;letter-spacing:-0.025em;color:${color.fg};padding-bottom:14px;">Confirm your place on the Waddle waitlist.</div>
								<div style="font-family:${font.body};font-size:16px;line-height:1.6;color:${color.muted};padding-bottom:24px;">
									We use a confirmation step so nobody can quietly add someone else’s inbox.
									If this request was yours, confirm it below. If not, cancel it with one click.
								</div>
								<div style="padding-bottom:20px;">
									${button("Confirm waitlist signup", input.confirmUrl, true)}
									${button("Cancel this request", input.unsubscribeUrl, false)}
								</div>
								<div style="font-family:${font.body};font-size:13px;line-height:1.7;color:${color.muted};word-break:break-word;border-top:1px solid ${color.borderSoft};padding-top:20px;">
									Prefer plain links?<br />
									<strong style="color:${color.fg};">Confirm:</strong>
									<a href="${confirmUrl}" style="color:${color.halo};text-decoration:underline;">${confirmUrl}</a><br />
									<strong style="color:${color.fg};">Cancel:</strong>
									<a href="${unsubscribeUrl}" style="color:${color.halo};text-decoration:underline;">${unsubscribeUrl}</a>
								</div>
							</td>
						</tr>
						<tr>
							<td style="padding:20px 4px 24px;font-family:${font.body};font-size:13px;line-height:1.6;color:${color.soft};">
								Waddle sends one confirmation email first, then only the launch notes you asked for.
								If you never requested this, use the cancel link and you’re done.
							</td>
						</tr>
						<tr>
							<td style="padding:0 4px 10px;font-family:${font.body};font-size:13px;line-height:18px;font-weight:700;letter-spacing:0.01em;color:${color.muted};">Supported by</td>
						</tr>
						${sponsor({
							href: "https://www.hetzner.com/?mtm_campaign=waddle-social&amp;mtm_medium=referral&amp;mtm_content=sponsoring_link",
							logo: asset("/hetzner-logo.jpg"),
							alt: "Hetzner",
							width: 84,
							title: "Infrastructure by Hetzner",
							caption: "Servers and hosting for waddle.social",
						})}
						${sponsor({
							href: "https://namespace.so",
							logo: asset("/namespace-logo.png"),
							alt: "Namespace",
							width: 32,
							title: "CI by Namespace Labs",
							caption: "Build runners and caches for Waddle’s CI",
						})}
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
		"",
		"Supported by Hetzner (infrastructure) and Namespace Labs (CI).",
	].join("\n");

	return { subject, html, text };
}
