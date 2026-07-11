// Public marketing pages: shared nav + footer, scoped stylesheet.

import Link from "next/link"
import "./marketing.css"

const NAV = [
  ["/pricing", "Pricing"],
  ["/templates", "Templates"],
  ["/docs/api", "API"],
  ["/docs/self-hosting", "Self-hosting"],
  ["/open-source", "Open source"],
  ["/login", "Sign in"],
] as const

const FOOTER: [string, [string, string][]][] = [
  ["Product", [
    ["/pricing", "Pricing"],
    ["/templates", "Template library"],
    ["/openapi.yaml", "OpenAPI spec"],
    ["/login", "Sign in"],
  ]],
  ["Developers", [
    ["/docs/api", "API guide"],
    ["/docs/self-hosting", "Self-hosting guide"],
    ["/open-source", "Open source"],
    ["https://github.com/camelmailer/camelmailer", "GitHub"],
  ]],
  ["Legal", [
    ["/legal/imprint", "Imprint"],
    ["/legal/privacy", "Privacy policy"],
    ["/legal/terms", "Terms of service"],
    ["/legal/dpa", "Data processing (DPA)"],
    ["/legal/acceptable-use", "Acceptable use"],
    ["/legal/sub-processors", "Sub-processors"],
  ]],
]

export default function MarketingLayout({ children }: { children: React.ReactNode }) {
  return (
    <div className="mkt">
      <div className="wrap">
        <header className="site">
          <Link className="logo" href="/">
            🐫 CamelMailer
          </Link>
          <nav className="site">
            {NAV.map(([href, label]) => (
              <Link key={href} href={href}>
                {label}
              </Link>
            ))}
          </nav>
        </header>
        {children}
        <footer className="site">
          <div className="cols">
            {FOOTER.map(([heading, links]) => (
              <div key={heading}>
                <h4>{heading}</h4>
                {links.map(([href, label]) => (
                  <a key={href} href={href}>
                    {label}
                  </a>
                ))}
              </div>
            ))}
          </div>
          <span>© 2026 CamelMailer. MIT licensed. Self-host it or let us run it.</span>
        </footer>
      </div>
    </div>
  )
}
