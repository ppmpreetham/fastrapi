import { Bloom, EffectComposer, Noise } from "@react-three/postprocessing"

import Experience from "./components/Experience"
import { Canvas } from "@react-three/fiber"
import { Suspense } from "react"
import { PerformanceMonitor } from "@react-three/drei"
import Cursor from "./components/Cursor"

export default function App() {
  return (
    <div className="w-screen min-h-screen h-screen cursor-none">
      <Cursor />
      <Canvas
        className="h-full w-full touch-auto"
        gl={{
          powerPreference: "high-performance",
          alpha: false,
        }}
        performance={{ min: 0.5 }}
        dpr={1}
        // frameloop="demand"

        // WEBGPU DOESNT WORK WELL WITH TEXT AND POSTPROCESSING YET
        // gl={(props) => {
        //   const renderer = new WebGPURenderer({
        //     canvas: props.canvas as HTMLCanvasElement,
        //     powerPreference: "high-performance",
        //     antialias: true,
        //     alpha: false,
        //   });
        //   return renderer.init().then(() => renderer);
        // }}
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
    </div>
  )
}
