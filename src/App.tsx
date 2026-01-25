import { Bloom, EffectComposer, Noise } from "@react-three/postprocessing"
import clsx from "clsx"
import Experience from "./components/Experience"
import { Canvas } from "@react-three/fiber"
import { Suspense, useState, useEffect } from "react"
import { PerformanceMonitor } from "@react-three/drei"
import Cursor from "./components/Cursor"
import { isMobile } from "./utils/helper"
import DocsTransition from "./components/DocsTransition"

export default function App() {
  const [showThreeJS, setShowThreeJS] = useState(true)

  useEffect(() => {
    const handler = () => {
      setShowThreeJS(false)
    }
    window.addEventListener("hideThreeJS", handler)
    return () => window.removeEventListener("hideThreeJS", handler)
  }, [])

  return (
    <div className={clsx("w-screen min-h-screen h-screen ", showThreeJS && "cursor-none")}>
      {!isMobile && showThreeJS && <Cursor />}
      <DocsTransition />
      {showThreeJS && (
        <Canvas
          className="h-full w-full touch-auto"
          gl={{
            powerPreference: "high-performance",
            alpha: true,
          }}
          performance={{ min: 0.5 }}
          dpr={1}
        >
          <PerformanceMonitor></PerformanceMonitor>
          <color attach="background" args={["#022cfd"]} />
          <Suspense>
            <Experience />
          </Suspense>
          <EffectComposer>
            <Bloom luminanceThreshold={0.1} mipmapBlur luminanceSmoothing={0.9} height={300} />
            <Noise opacity={0.15} />
          </EffectComposer>
        </Canvas>
      )}
    </div>
  )
}
