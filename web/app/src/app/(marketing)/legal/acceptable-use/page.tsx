import { MARKETING } from "../../content"

export const metadata = { title: MARKETING.legalAup.title }

export default function Page() {
  return <div dangerouslySetInnerHTML={{ __html: MARKETING.legalAup.html }} />
}
