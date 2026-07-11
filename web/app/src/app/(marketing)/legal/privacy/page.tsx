import { MARKETING } from "../../content"

export const metadata = { title: MARKETING.legalPrivacy.title }

export default function Page() {
  return <div dangerouslySetInnerHTML={{ __html: MARKETING.legalPrivacy.html }} />
}
