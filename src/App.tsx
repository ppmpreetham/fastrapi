import { Bloom, EffectComposer, Noise } from "@react-three/postprocessing";

import Experience from "./components/Experience";
import { Canvas } from "@react-three/fiber";

export default function App() {
  return (
    <div className="w-screen min-h-screen h-screen">
      <Canvas
        className="h-full w-full"
        shadows
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
        <color attach="background" args={["#022cfd"]} />
        <Experience />
        <EffectComposer>
          <Bloom luminanceThreshold={0} luminanceSmoothing={0.9} height={300} />
          <Noise opacity={0.15} />
        </EffectComposer>
      </Canvas>
    </div>
  );
}
