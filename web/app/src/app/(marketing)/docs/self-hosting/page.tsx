import { MARKETING } from "../../content"

export const metadata = { title: MARKETING.docsSelfHosting.title }

export default function Page() {
  return <div dangerouslySetInnerHTML={{ __html: MARKETING.docsSelfHosting.html }} />
}
