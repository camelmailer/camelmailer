import { MARKETING } from "../../content"

export const metadata = { title: MARKETING.legalTerms.title }

export default function Page() {
  return <div dangerouslySetInnerHTML={{ __html: MARKETING.legalTerms.html }} />
}
