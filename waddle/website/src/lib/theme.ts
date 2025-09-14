/**
 * Design System Theme Configuration
 * This file contains TypeScript constants for the design tokens defined in global.css
 */

// Spacing Scale (based on 8px)
export const spacing = {
	1: "0.5rem", // 8px
	2: "1rem", // 16px
	3: "1.5rem", // 24px
	4: "2rem", // 32px
	5: "2.5rem", // 40px
	6: "3rem", // 48px
	7: "3.5rem", // 56px
	8: "4rem", // 64px
	9: "4.5rem", // 72px
	10: "5rem", // 80px
	11: "5.5rem", // 88px
	12: "6rem", // 96px
} as const;

// Border Radius Tokens
export const radius = {
	sm: "0.25rem", // 4px
	md: "0.5rem", // 8px
	lg: "1rem", // 16px
} as const;

// Shadow Tokens
export const shadows = {
	light: {
		sm: "0 1px 2px 0 rgb(0 0 0 / 0.05)",
		md: "0 4px 6px -1px rgb(0 0 0 / 0.1), 0 2px 4px -2px rgb(0 0 0 / 0.1)",
		lg: "0 10px 15px -3px rgb(0 0 0 / 0.1), 0 4px 6px -4px rgb(0 0 0 / 0.1)",
	},
	dark: {
		sm: "0 1px 2px 0 rgb(0 0 0 / 0.25)",
		md: "0 4px 6px -1px rgb(0 0 0 / 0.3), 0 2px 4px -2px rgb(0 0 0 / 0.25)",
		lg: "0 10px 15px -3px rgb(0 0 0 / 0.35), 0 4px 6px -4px rgb(0 0 0 / 0.3)",
	},
} as const;

// Transition Duration Tokens
export const transitions = {
	fast: "150ms",
	base: "250ms",
	slow: "350ms",
	slower: "500ms",
} as const;

// Color Palette Configuration
export const colors = {
	light: {
		background: "#ffffff",
		foreground: "#0a0a0a",
		muted: "#f5f5f5",
		mutedForeground: "#737373",
		border: "#e5e5e5",

		primary: "#2563eb",
		primaryForeground: "#ffffff",
		primaryHover: "#1d4ed8",

		secondary: "#f3f4f6",
		secondaryForeground: "#1f2937",
		secondaryHover: "#e5e7eb",

		accent: "#8b5cf6",
		accentForeground: "#ffffff",
		accentHover: "#7c3aed",

		destructive: "#ef4444",
		destructiveForeground: "#ffffff",
		destructiveHover: "#dc2626",

		success: "#10b981",
		successForeground: "#ffffff",
		warning: "#f59e0b",
		warningForeground: "#ffffff",
	},
	dark: {
		background: "#0a0a0a",
		foreground: "#fafafa",
		muted: "#1a1a1a",
		mutedForeground: "#a3a3a3",
		border: "#262626",

		primary: "#3b82f6",
		primaryForeground: "#ffffff",
		primaryHover: "#60a5fa",

		secondary: "#1f2937",
		secondaryForeground: "#f3f4f6",
		secondaryHover: "#374151",

		accent: "#a78bfa",
		accentForeground: "#1f2937",
		accentHover: "#c4b5fd",

		destructive: "#f87171",
		destructiveForeground: "#1f2937",
		destructiveHover: "#fca5a5",

		success: "#34d399",
		successForeground: "#1f2937",
		warning: "#fbbf24",
		warningForeground: "#1f2937",
	},
} as const;

// Spacing Scale Utilities
export const space = (scale: keyof typeof spacing): string => {
	return spacing[scale];
};

// Get CSS Variable
export const getCSSVariable = (variable: string): string => {
	if (typeof window !== "undefined") {
		return getComputedStyle(document.documentElement)
			.getPropertyValue(`--${variable}`)
			.trim();
	}
	return "";
};

// Theme Mode Detection
export const getThemeMode = (): "light" | "dark" => {
	if (typeof window !== "undefined") {
		// Check for class-based dark mode first
		if (document.documentElement.classList.contains("dark")) {
			return "dark";
		}
		// Fall back to system preference
		if (window.matchMedia?.("(prefers-color-scheme: dark)").matches) {
			return "dark";
		}
	}
	return "light";
};

// Get current theme colors based on mode
export const getCurrentColors = () => {
	const mode = getThemeMode();
	return colors[mode];
};

// Type exports for better TypeScript support
export type SpacingScale = keyof typeof spacing;
export type RadiusScale = keyof typeof radius;
export type ShadowScale = keyof typeof shadows.light;
export type TransitionScale = keyof typeof transitions;
export type ColorMode = "light" | "dark";
export type ThemeColors = typeof colors.light;

// Complete theme object
export const theme = {
	spacing,
	radius,
	shadows,
	transitions,
	colors,
	// Utility functions
	space,
	getCSSVariable,
	getThemeMode,
	getCurrentColors,
} as const;

export default theme;
