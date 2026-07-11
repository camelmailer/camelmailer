import { MARKETING } from "../../content"

export const metadata = { title: MARKETING.legalSubs.title }

export default function Page() {
  return <div dangerouslySetInnerHTML={{ __html: MARKETING.legalSubs.html }} />
}
