import { useEffect, useRef, useState } from "react"
import gsap from "gsap"
import { useGSAP } from "@gsap/react"
import { useNavigate } from "react-router-dom"
import { isMobile } from "../utils/helper"

const DocsTransition = () => {
  const [fontSize, setFontSize] = useState("9rem")
  const docsButtonRef = useRef<HTMLDivElement>(null)
  const docsRef = useRef<HTMLAnchorElement>(null)
  const checkRef = useRef<HTMLSpanElement>(null)
  const [rainTriggered, setRainTriggered] = useState(false)
  const navigate = useNavigate()

  useEffect(() => {
    const calculateFontSize = () => {
      const threejsTextSize = isMobile ? 0.75 : 1.5
      const cameraDistance = 10
      const fovRadians = (75 * Math.PI) / 180
      const visibleHeight = 2 * Math.tan(fovRadians / 2) * cameraDistance
      const pixelsPerUnit = window.innerHeight / visibleHeight
      setFontSize(`${threejsTextSize * 1.65 * pixelsPerUnit}px`)
    }
    calculateFontSize()
    window.addEventListener("resize", calculateFontSize)
    return () => window.removeEventListener("resize", calculateFontSize)
  }, [])

  const animateToDocsState = () => {
    const tl = gsap.timeline()

    tl.to(docsButtonRef.current, {
      width: "100vw",
      height: "100vh",
      duration: 1.5,
      ease: "power2.inOut",
      onComplete: () => {
        window.dispatchEvent(new Event("hideThreeJS"))
      },
    })
      .to(checkRef.current, {
        opacity: 0,
        duration: 0.5,
        onComplete: () => {
          if (checkRef.current) checkRef.current.innerText = "FastRAPI"
        },
      })
      .to([checkRef.current, docsRef.current], {
        opacity: 1,
        color: "#c9ff61",
        duration: 1,
      })
      .to(
        docsButtonRef.current,
        {
          backgroundColor: "#000000",
          duration: 1,
        },
        "<",
      )
      .add(() => {
        navigate("/docs")
      })
  }

  useGSAP(() => {
    gsap.set(docsButtonRef.current, { opacity: 0 })

    const handler = () => {
      if (rainTriggered) return
      setRainTriggered(true)

      gsap.to(docsButtonRef.current, {
        opacity: 1,
        duration: 1,
        onComplete: () => {
          setTimeout(() => animateToDocsState(), 2500)
        },
      })
    }

    window.addEventListener("triggerRainDocs", handler)
    return () => window.removeEventListener("triggerRainDocs", handler)
  }, [rainTriggered])

  return (
    <div className="fixed border-none z-10 w-screen h-screen flex justify-center items-center font-random pointer-events-none">
      <div
        ref={docsButtonRef}
        className="bg-primary-glow text-black flex justify-center items-center overflow-hidden"
        style={{ fontSize }}
      >
        <a ref={docsRef}>
          <span ref={checkRef}>Check</span> <span>Docs</span>
        </a>
      </div>
    </div>
  )
}

export default DocsTransition
