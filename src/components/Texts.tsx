import { Text } from "@react-three/drei";
const PI = Math.PI;
const front = 0.01;
const back = -0.92;

export const HeaderText = () => {
  return (
    <>
      <Text color="#c9ff61" fontSize={2} font="/fonts/random.ttf" position={[0, 0, front]}>
        FastRAPI
      </Text>
      <Text color="black" fontSize={2} font="/fonts/random.ttf" position={[0.05, -0.05, 0]}>
        FastRAPI
      </Text>
      <Text
        color="#c9ff61"
        fontSize={2}
        font="/fonts/random.ttf"
        position={[0, 0, back]}
        rotation={[0, PI, 0]}
      >
        FastRAPI
      </Text>
      <Text color="black" fontSize={2} font="/fonts/random.ttf" position={[0.05, -0.05, 0]}>
        FastRAPI
      </Text>
    </>
  );
};
