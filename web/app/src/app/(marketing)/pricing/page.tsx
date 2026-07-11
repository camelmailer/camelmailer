import { MARKETING } from "../content"

export const metadata = { title: MARKETING.pricing.title }

export default function Page() {
  return <div dangerouslySetInnerHTML={{ __html: MARKETING.pricing.html }} />
}
