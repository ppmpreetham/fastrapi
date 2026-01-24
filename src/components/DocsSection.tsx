import { useEffect, useRef, useState } from "react"
import gsap from "gsap"
import { useGSAP } from "@gsap/react"
import { ScrollTrigger, Flip } from "gsap/all"
import { isMobile } from "../utils/helper"

gsap.registerPlugin(ScrollTrigger)
const DocsSection = () => {
  const [fontSize, setFontSize] = useState("9rem")

  useEffect(() => {
    const calculateFontSize = () => {
      const threejsTextSize = isMobile ? 0.75 : 1.5
      const cameraDistance = 10
      const cameraFov = 75

      const fovRadians = (cameraFov * Math.PI) / 180
      const visibleHeight = 2 * Math.tan(fovRadians / 2) * cameraDistance
      const pixelsPerUnit = window.innerHeight / visibleHeight

      const textHeightInUnits = threejsTextSize * 1.65
      const textHeightInPixels = textHeightInUnits * pixelsPerUnit

      setFontSize(`${textHeightInPixels}px`)
    }
    calculateFontSize()
    window.addEventListener("resize", calculateFontSize)

    return () => window.removeEventListener("resize", calculateFontSize)
  }, [])

  const docsButtonRef = useRef<HTMLDivElement>(null)
  const [rainTriggered, setRainTriggered] = useState(false)

  useGSAP(() => {
    gsap.set(docsButtonRef.current, { opacity: 0 })
    const handler = () => {
      if (rainTriggered) return
      setRainTriggered(true)
      gsap.to(docsButtonRef.current, {
        opacity: 1,
        duration: 3.5,
        ease: "power2.out",
      })
    }
    window.addEventListener("triggerRainDocs", handler)
    return () => window.removeEventListener("triggerRainDocs", handler)
  }, [rainTriggered])

  return (
    <div className="fixed z-10 w-screen h-screen flex justify-center items-center font-random pointer-events-none will-change-transform">
      {" "}
      {/*navbar items */}
      <div ref={docsButtonRef} className="fixed bg-primary-glow text-black" style={{ fontSize }}>
        Check Docs
      </div>
      {/* <div className="flex justify-between items-start">
        <div className="navbar-links">
          <a href="/">
            <img src="/logo.png" alt="Logo" />
          </a>
          <a href="#">Link1</a>
          <a href="#">Link2</a>
        </div>
        <div className="text-[clamp(3rem, 5vw, 7rem)]">
          <h1>eorighioerjgioerjg</h1>
        </div>
        <div className="text-[clamp(3rem, 5vw, 7rem)]">
          <h1>eorighioerjgioerjg</h1>
        </div>
      </div> */}
    </div>
  )
}

export default DocsSection
