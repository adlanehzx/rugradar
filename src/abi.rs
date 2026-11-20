//! Solidity interfaces used by the scanner (PancakeSwap V2 is a Uniswap V2 fork).
use alloy::sol;

sol! {
    #[sol(rpc)]
    interface IPancakeFactory {
        event PairCreated(address indexed token0, address indexed token1, address pair, uint256);
        function getPair(address tokenA, address tokenB) external view returns (address pair);
    }

    #[sol(rpc)]
    interface IPancakePair {
        function getReserves() external view returns (uint112 reserve0, uint112 reserve1, uint32 blockTimestampLast);
        function totalSupply() external view returns (uint256);
        function balanceOf(address owner) external view returns (uint256);
    }

    #[sol(rpc)]
    interface IERC20Meta {
        function name() external view returns (string);
        function symbol() external view returns (string);
        function decimals() external view returns (uint8);
        function totalSupply() external view returns (uint256);
    }

    #[sol(rpc)]
    interface IOwnable {
        function owner() external view returns (address);
    }

    /// Mirror of contracts/Simulator.sol. The contract is never deployed: its runtime bytecode
    /// is injected with an `eth_call` state override.
    interface ISimulator {
        struct SimResult {
            uint8 status;
            uint256 buyExpected;
            uint256 buyReceived;
            uint256 sellExpected;
            uint256 sellReceived;
            uint256 transferSent;
            uint256 transferReceived;
            uint256 buyGas;
            uint256 sellGas;
        }

        function simulate(address router, address token, address wrappedNative)
            external
            payable
            returns (SimResult memory r);
    }
}
