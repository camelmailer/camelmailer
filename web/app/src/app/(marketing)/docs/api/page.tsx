import { MARKETING } from "../../content"

export const metadata = { title: MARKETING.docsApi.title }

export default function Page() {
  return <div dangerouslySetInnerHTML={{ __html: MARKETING.docsApi.html }} />
}
