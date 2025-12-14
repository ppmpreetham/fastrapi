import { OrbitControls, ScrollControls } from "@react-three/drei";
import { Bloom, DepthOfField, EffectComposer, Noise, Vignette } from "@react-three/postprocessing";
import { FastrapiModel } from "./models/fastrapimodel";

import { Canvas, extend } from "@react-three/fiber";
import * as THREE from "three";
// import { WebGPURenderer } from "three/webgpu";
import { Text } from "@react-three/drei";

export default function App() {
  return (
    <div className="w-screen min-h-screen h-screen">
      <Canvas className="h-full w-full bg-secondary">
        <OrbitControls />
        <FastrapiModel />
        {/* <Text font={"/fonts/random.json"} position={[-1, 0, 0]}>
          Sample Text
        </Text> */}
        <Text color="#c9ff61" fontSize={2} font="/fonts/random.ttf" position={[0, 0, 0.1]}>
          FastRAPI
        </Text>
        <Text color="black" fontSize={2} font="/fonts/random.ttf" position={[0.05, -0.05, 0.09]}>
          FastRAPI
        </Text>
        <EffectComposer>
          {/* <DepthOfField focusDistance={0} focalLength={0.02} bokehScale={2} height={480} /> */}
          {/* <Bloom luminanceThreshold={0} luminanceSmoothing={0.9} height={300} /> */}
          <Noise opacity={0.2} />
          {/* <Vignette eskil={false} offset={0.1} darkness={1.1} /> */}
        </EffectComposer>
      </Canvas>
    </div>
  );
}
