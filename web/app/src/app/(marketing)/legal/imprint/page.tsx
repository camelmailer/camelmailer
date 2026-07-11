import { MARKETING } from "../../content"

export const metadata = { title: MARKETING.legalImprint.title }

export default function Page() {
  return <div dangerouslySetInnerHTML={{ __html: MARKETING.legalImprint.html }} />
}
