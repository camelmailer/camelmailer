import { MARKETING } from "../../content"

export const metadata = { title: MARKETING.legalDpa.title }

export default function Page() {
  return <div dangerouslySetInnerHTML={{ __html: MARKETING.legalDpa.html }} />
}
