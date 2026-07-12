import type { Metadata } from "next"
import "./globals.css"
import Providers from "./providers"

export const metadata: Metadata = {
  title: { default: "CamelMailer", template: "%s — CamelMailer" },
  description:
    "Transactional email, nothing else. Self-hosted or EU cloud. Open source, MIT.",
  icons: {
    icon: [
      { url: "/favicon.ico", sizes: "any" },
      { url: "/favicon.svg", type: "image/svg+xml" },
      { url: "/favicon-96x96.png", type: "image/png", sizes: "96x96" },
    ],
    apple: "/apple-touch-icon.png",
  },
  manifest: "/site.webmanifest",
}

// Applies the stored theme (see src/components/theme.tsx — key
// "camelmailer.theme") before first paint to avoid a light-mode flash.
const THEME_INIT_SCRIPT = `try{var t=localStorage.getItem("camelmailer.theme");var d=t==="dark"||(t!=="light"&&window.matchMedia("(prefers-color-scheme: dark)").matches);document.documentElement.classList.toggle("dark",d)}catch(e){}`

export default function RootLayout({ children }: { children: React.ReactNode }) {
  return (
    <html lang="en" suppressHydrationWarning>
      <head>
        <link rel="preconnect" href="https://fonts.bunny.net" />
        <link
          rel="stylesheet"
          href="https://fonts.bunny.net/css?family=dm-sans:400,500,600,700&display=swap"
        />
      </head>
      <body className="font-sans antialiased">
        <script dangerouslySetInnerHTML={{ __html: THEME_INIT_SCRIPT }} />
        <Providers>{children}</Providers>
      </body>
    </html>
  )
}
