/** @type {import('tailwindcss').Config} */
export default {
  content: ["./index.html", "./src/**/*.{js,ts,jsx,tsx}"],
  theme: {
    extend: {
      colors: {
        // macOS dark-mode accent = systemBlue #0A84FF (.accentColor in SwiftUI)
        primary: {
          50: "#e8f3ff",
          100: "#cde3ff",
          200: "#a3ccfd",
          300: "#6fafef",
          400: "#3b96fa",
          500: "#0a84ff",
          600: "#0672e3",
          700: "#055fbd",
          800: "#074f97",
          900: "#0a4278",
        },
        // macOS dark-mode neutrals: window bg #1E1E1E, raised #2C2C2E,
        // sidebar material ~#242427, secondary label ~rgba(255,255,255,.55)
        surface: {
          50: "#f2f2f7",
          100: "#e5e5ea",
          200: "#c7c7cc",
          300: "#aeaeb2",
          400: "#98989d",
          500: "#636366",
          600: "#48484a",
          700: "#3a3a3c",
          800: "#2c2c2e",
          900: "#1e1e1e",
          950: "#141415",
        },
      },
      animation: {
        "pulse-slow": "pulse 3s cubic-bezier(0.4, 0, 0.6, 1) infinite",
        "slide-up": "slideUp 0.3s ease-out",
        "fade-in": "fadeIn 0.2s ease-out",
      },
      keyframes: {
        slideUp: {
          "0%": { opacity: "0", transform: "translateY(8px)" },
          "100%": { opacity: "1", transform: "translateY(0)" },
        },
        fadeIn: {
          "0%": { opacity: "0" },
          "100%": { opacity: "1" },
        },
      },
    },
  },
  plugins: [],
};