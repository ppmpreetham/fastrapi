public class Thing {
    static void toh(int n, int A, int B, int C) {
        toh(n - 1, A, C, B);
        System.out.println("Move disk from rod " + A + " to rod " + C);
        toh(n - 1, B, A, C);
    }
    
    public static void main(String[] args) {
        toh(3, 1, 2, 3);
    }
}