import type { Metadata } from "next"
import "./globals.css"
import Providers from "./providers"

export const metadata: Metadata = {
  title: { default: "CamelMailer", template: "%s — CamelMailer" },
  description:
    "Transactional email, nothing else. Self-hosted or EU cloud. Open source, MIT.",
  icons: {
    icon: 'data:image/svg+xml,<svg xmlns=%22http://www.w3.org/2000/svg%22 viewBox=%220 0 100 100%22><text y=%22.9em%22 font-size=%2290%22>🐫</text></svg>',
  },
}

// Applies the stored theme (see src/components/theme.tsx — key
// "camelmailer.theme") before first paint to avoid a light-mode flash.
const THEME_INIT_SCRIPT = `try{var t=localStorage.getItem("camelmailer.theme");var d=t==="dark"||(t!=="light"&&window.matchMedia("(prefers-color-scheme: dark)").matches);document.documentElement.classList.toggle("dark",d)}catch(e){}`

export default function RootLayout({ children }: { children: React.ReactNode }) {
  return (
    <html lang="en" suppressHydrationWarning>
      <body className="antialiased">
        <script dangerouslySetInnerHTML={{ __html: THEME_INIT_SCRIPT }} />
        <Providers>{children}</Providers>
      </body>
    </html>
  )
}
