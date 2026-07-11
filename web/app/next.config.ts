import type { NextConfig } from "next"

// The CamelMailer backend the Next server proxies API calls to. In
// development this is the local Docker stack; in production point it at
// your instance (the proxy makes the app same-origin — no CORS needed).
const API_URL = process.env.API_PROXY_URL ?? "http://localhost:5000"

const nextConfig: NextConfig = {
  async rewrites() {
    return [
      { source: "/api/:path*", destination: `${API_URL}/api/:path*` },
      { source: "/health", destination: `${API_URL}/health` },
    ]
  },
}

export default nextConfig
